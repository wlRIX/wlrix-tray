// SPDX-License-Identifier: GPL-3.0-or-later
//! The tray, as a Wayland client.
//!
//! One **wlr-layer-shell bottom surface**, anchored to a corner, exactly as big as the strip it
//! draws. `Bottom` and not `Top`: IRIX's tray was an object on the desktop rather than a panel
//! over it, so windows cover it, and it takes **no exclusive zone** -- nobody's work area shrinks
//! because a program started publishing a status icon.
//!
//! `calloop` drives everything: the Wayland connection, the channel the D-Bus threads report on,
//! and the `SIGHUP` reload ping are all sources on one loop, so an item appearing and a pointer
//! moving arrive the same way and nothing polls.
//!
//! # Resizing is the whole job
//!
//! Unlike `wlrix-desktop`, whose surface is the screen, this one changes size constantly -- every
//! item that comes or goes, every menu opened or closed. Two rules follow, and breaking either is
//! fatal rather than untidy:
//!
//! - **Nothing may be attached before the compositor has configured the surface.** A buffer
//!   attached early is a protocol error, which kills the connection. `configured` gates the paint
//!   and `dirty` stays set, so a paint held back happens from `configure` instead.
//! - **The surface is exactly the size of what is drawn.** `wlrix-desktop` is a fullscreen
//!   bottom-layer surface, and the compositor's `layer_under` picks the topmost bottom-layer
//!   surface *by bounding box*; when its input region rejects the point it falls through to the
//!   **background** layer, not to the desktop underneath. A transparent margin around the tray
//!   would therefore swallow clicks that belong to the desktop icons. See [`crate::layout`].
//!
//! The same ordering fact runs the other way: the tray must be started *after* `wlrix-desktop`, so
//! it maps later and sorts above it within the bottom layer. `wlrix-session`'s `DEFAULT_APPS` puts
//! it there.
//!
//! # Menus live in this surface, not in an `xdg_popup`
//!
//! `wlrix-compositor` never unconstrains a popup whose root is a layer surface -- it looks the
//! root up in its window space and returns early -- so a tray popup would not be kept on screen;
//! and popup grabs are a standing TODO there, so `xdg_popup.grab()` would not dismiss on an
//! outside click. Growing this surface needs no compositor change and is what `wlrix-desktop`
//! already does. The cost is that while a menu is open the surface covers the corner between the
//! menu and the strip, and a click there dismisses the menu rather than reaching the desktop --
//! which is what a menu grab does everywhere else.

pub mod paint;

use std::collections::HashMap;
use std::time::Duration;

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::calloop::EventLoop,
    reexports::calloop_wayland_source::WaylandSource,
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor as ShellAnchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler,
            LayerSurface, LayerSurfaceConfigure,
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use wayland_client::{
    Connection, QueueHandle,
    backend::WaylandError,
    globals::registry_queue_init,
    protocol::{
        wl_keyboard::WlKeyboard, wl_output::WlOutput, wl_pointer::WlPointer, wl_seat::WlSeat,
        wl_shm, wl_surface::WlSurface,
    },
};
use wlrix_ui::canvas::Canvas;
use wlrix_ui::palette::Palette;
use wlrix_ui::text::{Face, Fonts};

use crate::config::Config;
use crate::icons::Icons;
use crate::layout::{Anchor, Frame, Metrics, Orientation};
use crate::menu::Posted;
use crate::model::Items;
use crate::pixmap::{self, Pixmap};
use crate::sni::{Axis, Bus, Command, Invocation, ItemAddress, Status, TrayEvent};
use crate::tooltip::Tooltip;

/// `wl_pointer`'s buttons, from `linux/input-event-codes.h`.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

/// How long the pointer has to rest on a cell before its tooltip appears.
///
/// Long enough that sweeping across the strip on the way somewhere else shows nothing, short
/// enough that stopping to ask what a cell is gets an answer.
const TOOLTIP_DELAY: Duration = Duration::from_millis(600);

/// How often the loop wakes while waiting for [`TOOLTIP_DELAY`] to elapse.
///
/// The tooltip is checked after each dispatch rather than from a timer source. Pointer motion
/// wakes the loop on its own; what needs a wake-up is the pointer having *stopped*, and shortening
/// the idle timeout while a cell is hovered is a smaller thing than a second source and a handle
/// to register it from.
const HOVER_TICK: Duration = Duration::from_millis(100);

/// How long the loop sleeps when nothing is being pointed at.
const IDLE_TICK: Duration = Duration::from_secs(1);

/// How much continuous scrolling counts as one notch.
///
/// A wheel sends discrete steps and a touchpad sends a stream of small ones. `Scroll` takes a
/// number the item interprets however it likes, and fcitx5 treats any non-zero delta as "next
/// input method" -- so forwarding every touchpad fragment would cycle through every method
/// installed in the time it takes to notice.
const SCROLL_NOTCH: f64 = 10.0;

/// One item's artwork, decoded and scaled to the current cell size.
///
/// Kept here rather than worked out while painting: an `IconPixmap` has to be unpacked from
/// big-endian, premultiplied and scaled, and doing that for every item on every hover would be
/// the most expensive thing the tray does. Rebuilt when a read lands and when the config reloads.
#[derive(Default)]
struct Artwork {
    icon: Option<Pixmap>,
    overlay: Option<Pixmap>,
}

/// The whole tray's state.
pub struct Tray {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    shm: Shm,
    compositor: CompositorState,
    layer_shell: LayerShell,
    pool: SlotPool,

    /// The strip surface, once there is something to put in it.
    layer: Option<LayerSurface>,
    /// Which output it is on. Kept so an unplug can be told from any other output going.
    output: Option<WlOutput>,
    /// That output's logical size, for keeping a menu on screen.
    output_size: Option<(i32, i32)>,
    /// Whether the compositor has configured `layer` yet. Attaching a buffer before the first
    /// configure is acked is a protocol error, which kills the connection.
    configured: bool,
    /// The size the compositor last configured, which is what is drawn at.
    width: u32,
    height: u32,
    /// The size last asked for, so a resize is requested once rather than every frame.
    requested: (u32, u32),

    pointer: Option<WlPointer>,
    keyboard: Option<WlKeyboard>,
    modifiers: Modifiers,

    fonts: Fonts,
    icons: Icons,
    config: Config,
    /// The color scheme everything is drawn in, resolved from `config` at load and on every
    /// reload. `&'static`, because every scheme is baked into `wlrix-ui`.
    palette: &'static Palette,
    metrics: Metrics,
    anchor: Anchor,
    orientation: Orientation,

    bus: Bus,
    items: Items,
    /// Decoded artwork, one entry per known item.
    artwork: HashMap<ItemAddress, Artwork>,
    /// The items on the strip, in strip order. Recomputed by [`Tray::refresh`]; a cell index is an
    /// index into this.
    shown: Vec<ItemAddress>,

    /// Where everything goes this frame.
    frame: Frame,
    /// The posted menu, and which cell it hangs off.
    menu: Option<Posted>,
    menu_owner: Option<ItemAddress>,
    menu_at: usize,

    hovered: Option<usize>,
    /// When the pointer settled on `hovered`, so the tooltip knows when it is due. `None` once the
    /// tooltip has been shown or ruled out, so it is worked out once per hover rather than every
    /// time round the loop.
    hovered_since: Option<std::time::Instant>,
    /// The hover tip on screen. Shares the frame's popup slot with a posted menu.
    tooltip: Option<Tooltip>,
    pressed: Option<usize>,
    /// Scroll not yet worth a notch, per axis.
    scrolled: (f64, f64),

    dirty: bool,
    exit: bool,
}

/// Run the tray until the compositor goes away.
pub fn run(replace: bool) -> Result<(), String> {
    let config = Config::load();
    let metrics = config.metrics.resolve();
    let (palette, unknown) = wlrix_ui::palette::resolve(config.appearance.palette.as_deref());
    if let Some(wlrix_ui::palette::UnknownPalette(name)) = unknown {
        eprintln!(
            "wlrix-tray: no color scheme called {name:?}; using {}",
            palette.id
        );
    }
    let fonts = Fonts::load()?;
    let mut icons = Icons::default();
    icons.set_theme(config.appearance.icon_theme());

    // The bus first. Without the watcher name there is nothing to draw, ever, so failing here is
    // failing to start -- and it is the one failure worth refusing to run over.
    let (bus, events) = crate::sni::spawn(replace)?;
    eprintln!(
        "wlrix-tray: watching {} on the session bus, labels in {} ({} faces), scheme {}",
        crate::sni::WATCHER_NAME,
        fonts.family(),
        fonts.face_count(),
        palette.id,
    );

    let conn = Connection::connect_to_env()
        .map_err(|err| format!("no Wayland compositor to connect to: {err}"))?;
    let (globals, event_queue) =
        registry_queue_init(&conn).map_err(|err| format!("could not read the registry: {err}"))?;
    let qh = event_queue.handle();

    let mut event_loop: EventLoop<Tray> =
        EventLoop::try_new().map_err(|err| format!("could not create the event loop: {err}"))?;
    let loop_handle = event_loop.handle();

    let compositor = CompositorState::bind(&globals, &qh)
        .map_err(|err| format!("wl_compositor unavailable: {err}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh)
        .map_err(|err| format!("wlr-layer-shell unavailable: {err}"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|err| format!("wl_shm unavailable: {err}"))?;
    // A tray is small. This holds a strip plus a generous menu; the pool grows on demand if a
    // menu ever wants more.
    let pool = SlotPool::new(512 * 512 * 4, &shm)
        .map_err(|err| format!("could not create a buffer pool: {err}"))?;

    let anchor = config.anchor;
    let orientation = config.orientation;
    let mut tray = Tray {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        shm,
        compositor,
        layer_shell,
        pool,
        layer: None,
        output: None,
        output_size: None,
        configured: false,
        width: 0,
        height: 0,
        requested: (0, 0),
        pointer: None,
        keyboard: None,
        modifiers: Modifiers::default(),
        fonts,
        icons,
        config,
        palette,
        metrics,
        anchor,
        orientation,
        bus,
        items: Items::default(),
        artwork: HashMap::new(),
        shown: Vec::new(),
        frame: Frame::new(0, anchor, orientation, metrics, None, 0, None),
        menu: None,
        menu_owner: None,
        menu_at: 0,
        hovered: None,
        hovered_since: None,
        tooltip: None,
        pressed: None,
        scrolled: (0.0, 0.0),
        dirty: false,
        exit: false,
    };

    // Kept for the loop below, to tell a compositor that has gone away from a real failure.
    let health = conn.clone();
    WaylandSource::new(conn, event_queue)
        .insert(loop_handle.clone())
        .map_err(|err| format!("could not drive Wayland from the loop: {err}"))?;

    // Everything the bus threads have to say, on the same loop as everything else.
    let qh_for_events = qh.clone();
    loop_handle
        .insert_source(events, move |event, _, tray: &mut Tray| {
            if let calloop::channel::Event::Msg(event) = event {
                tray.on_bus(&qh_for_events, event);
            }
        })
        .map_err(|err| format!("could not listen to the bus: {err}"))?;

    // Re-read `tray.toml` on `SIGHUP`, so a settings change applies without restarting the tray.
    // `wlrix-settings-daemon` finds this process through the pidfile below; `kill -HUP` does the
    // same thing by hand.
    let (reload_ping, reload_source) = calloop::ping::make_ping()
        .map_err(|err| format!("could not create the reload ping: {err}"))?;
    let qh_for_reload = qh.clone();
    loop_handle
        .insert_source(reload_source, move |_, _, tray: &mut Tray| {
            tray.reload_config(&qh_for_reload);
        })
        .map_err(|err| format!("could not watch for reloads: {err}"))?;
    crate::signals::forward_reload_to_loop(reload_ping);

    // Held until `run` returns, then its guard removes the file -- so a live pidfile means a live
    // tray.
    let _pidfile = crate::pidfile::write();

    // The surface needs an output, which arrives from the loop; one dispatch settles the output
    // list, and `new_output` covers anything that shows up later.
    event_loop
        .dispatch(Duration::from_millis(200), &mut tray)
        .map_err(|err| format!("initial dispatch failed: {err}"))?;
    tray.refresh(&qh);

    loop {
        // Short while a cell is being pointed at and its tooltip is still pending, so the tip
        // appears when the pointer stops rather than up to a second later; long otherwise, so an
        // idle tray wakes once a second.
        let timeout = if tray.tooltip_pending() {
            HOVER_TICK
        } else {
            IDLE_TICK
        };
        if let Err(err) = event_loop.dispatch(timeout, &mut tray) {
            return match health.flush() {
                // The compositor going away is the ordinary end of a Wayland client's life, not a
                // failure: logging out looks exactly like this, and reporting it would put an
                // error in the session log every single time.
                Err(WaylandError::Io(_)) => Ok(()),
                _ => Err(format!("event loop failed: {err}")),
            };
        }
        // A protocol error is this program's own bug, and it has to be caught here by hand:
        // `calloop-wayland-source` matches only `WaylandError::Io` on all three of its error
        // paths, and `EventQueue::dispatch_pending` drops the error outright, so the dispatch
        // above returns `Ok`. Meanwhile the socket stays readable forever, so the loop would spin
        // at 100% of a core for the rest of the session, silently, with nothing on screen.
        if let Some(err) = health.protocol_error() {
            return Err(format!("protocol error: {err}"));
        }
        tray.tick_tooltip(&qh);
        // Painted after each batch rather than on a frame callback: the tray redraws on demand --
        // a hover, a click, an item arriving -- and a frame callback only arrives after a commit
        // that asked for one, so waiting on it stalls once nothing is moving.
        tray.draw_if_dirty();
        if tray.exit {
            return Ok(());
        }
    }
}

impl Tray {
    /// Act on one thing the bus said.
    fn on_bus(&mut self, qh: &QueueHandle<Self>, event: TrayEvent) {
        match event {
            TrayEvent::Registered(address) => {
                if self.items.register(address.clone()) {
                    // Nothing is drawn for it yet: an item becomes visible when its first read
                    // lands. See `crate::model`.
                    self.bus.send(Command::Read(address));
                }
            }
            TrayEvent::Gone(address) => {
                if !self.items.remove(&address) {
                    return;
                }
                self.artwork.remove(&address);
                if self.menu_owner.as_ref() == Some(&address) {
                    self.close_menu();
                }
                self.refresh(qh);
            }
            TrayEvent::Properties(address, properties) => {
                let label = properties.label().to_owned();
                if !self.items.update(&address, *properties) {
                    return;
                }
                eprintln!("wlrix-tray: {label} ({})", address.service);
                self.render(&address);
                self.refresh(qh);
            }
            TrayEvent::Changed(address) => {
                // Every change signal means the same thing -- see `crate::sni::worker`. The icon
                // cache is cleared for this item because the *name* may be unchanged while the
                // file behind it is not, which is how a themed icon changes state.
                self.icons.clear();
                self.bus.send(Command::Read(address));
            }
            TrayEvent::Menu(address, menu) => self.post_menu(qh, address, menu),
            TrayEvent::MenuStale(address) => {
                // Only worth acting on for the menu actually on screen. An item rebuilding a menu
                // nobody is looking at is ordinary and costs nothing.
                if self.menu_owner.as_ref() != Some(&address) {
                    return;
                }
                if let Some(path) = self.menu.as_ref().map(|posted| posted.menu.path.clone()) {
                    self.bus.send(Command::OpenMenu(address, path));
                }
            }
        }
    }

    /// Decode an item's artwork at the current cell size.
    ///
    /// `IconName` is preferred over `IconPixmap` when it resolves: a themed name gives artwork at
    /// the size asked for, where a pixmap is usually a 22-pixel bitmap that has to be scaled.
    fn render(&mut self, address: &ItemAddress) {
        let Some(item) = self.items.get(address) else {
            return;
        };
        let size = self.metrics.icon;
        let properties = &item.properties;
        let theme_path = properties.icon_theme_path.clone();
        let name = properties.current_icon_name().to_owned();
        let overlay_name = properties.overlay_icon_name.clone();
        let pixmaps = properties.current_pixmaps().to_vec();
        let overlay_pixmaps = properties.overlay_icon_pixmap.clone();

        let icon = self
            .icons
            .get(&theme_path, &name, size)
            .cloned()
            .or_else(|| pixmap::best(&pixmaps, size).map(|pixmap| pixmap.scaled(size)));

        // Half size, which is where every implementation of this puts it.
        let overlay_size = (size / 2).max(1);
        let overlay = self
            .icons
            .get(&theme_path, &overlay_name, overlay_size)
            .cloned()
            .or_else(|| {
                pixmap::best(&overlay_pixmaps, overlay_size)
                    .map(|pixmap| pixmap.scaled(overlay_size))
            });

        self.artwork
            .insert(address.clone(), Artwork { icon, overlay });
    }

    /// Put a fetched menu on screen.
    fn post_menu(
        &mut self,
        qh: &QueueHandle<Self>,
        address: ItemAddress,
        menu: Result<crate::sni::dbusmenu::Menu, String>,
    ) {
        // A menu that arrives after the pointer moved on, or after the item went away, is not put
        // on screen: it would appear under the pointer with no warning.
        if self.menu_owner.as_ref() != Some(&address) {
            return;
        }
        let menu = match menu {
            Ok(menu) => menu,
            Err(err) => {
                eprintln!("wlrix-tray: {err}");
                self.close_menu();
                self.refresh(qh);
                return;
            }
        };
        if menu.is_empty() {
            // Posting it would put an empty beveled box on the desktop, which reads as a bug
            // rather than as "this application offers nothing here".
            self.close_menu();
            self.refresh(qh);
            return;
        }

        let header = self
            .items
            .get(&address)
            .map(|item| item.properties.label().to_owned())
            .unwrap_or_default();
        let fonts = &mut self.fonts;
        let measure = |label: &str| fonts.width(Face::Bold, crate::menu::LABEL_PX, label);
        self.menu = Some(Posted::new(menu, &header, measure));
        self.refresh(qh);
    }

    /// Ask an item for its menu, or fall back to its own `ContextMenu`.
    fn open_menu(&mut self, qh: &QueueHandle<Self>, cell: usize) {
        let Some(address) = self.shown.get(cell).cloned() else {
            return;
        };
        let path = self
            .items
            .get(&address)
            .and_then(|item| item.properties.menu.clone());
        self.menu = None;
        self.clear_tooltip();
        self.menu_at = cell;
        match path {
            Some(path) => {
                self.menu_owner = Some(address.clone());
                self.bus.send(Command::OpenMenu(address, path));
            }
            None => {
                // No dbusmenu object. `ContextMenu` is what the specification offers instead, and
                // on Wayland most applications answer it by doing nothing -- but an application
                // that does put its own window up can only be reached this way.
                self.menu_owner = None;
                let (x, y) = self.screen_position(cell);
                self.bus
                    .send(Command::Invoke(address, Invocation::ContextMenu, x, y));
            }
        }
        self.refresh(qh);
    }

    fn close_menu(&mut self) {
        self.menu = None;
        self.menu_owner = None;
    }

    /// Roughly where a cell is on screen, for the methods that take a position.
    ///
    /// Roughly, and that is fine: an item uses it to decide where to put a window of its own, and
    /// the strip's own corner is close enough for that. Wayland gives a client no way to learn
    /// where its surface actually is, so an exact answer is not available at any price.
    fn screen_position(&self, cell: usize) -> (i32, i32) {
        let rect = self.frame.cell(cell).unwrap_or(self.frame.strip);
        let (screen_w, screen_h) = self.output_size.unwrap_or((0, 0));
        let margin = self.metrics.margin;
        let origin_x = match self.anchor {
            Anchor::BottomLeft | Anchor::TopLeft => margin,
            Anchor::BottomRight | Anchor::TopRight => screen_w - margin - self.frame.width,
        };
        let origin_y = match self.anchor {
            Anchor::TopLeft | Anchor::TopRight => margin,
            Anchor::BottomLeft | Anchor::BottomRight => screen_h - margin - self.frame.height,
        };
        (
            origin_x + rect.x + rect.w / 2,
            origin_y + rect.y + rect.h / 2,
        )
    }

    /// Work out the layout, resize the surface if it changed, and ask for a paint.
    ///
    /// The one place the strip's size is decided, so that "an item arrived", "a menu opened" and
    /// "the config changed" all take the same path to the screen.
    fn refresh(&mut self, qh: &QueueHandle<Self>) {
        self.shown = self
            .items
            .visible(&self.config)
            .iter()
            .map(|item| item.address.clone())
            .collect();

        // Nothing to show. A `wl_surface` cannot be zero-sized and an invisible one would still
        // take clicks, so the surface goes away entirely rather than shrinking to a dot.
        if self.shown.is_empty() && self.config.hide_when_empty() {
            self.close_menu();
            self.clear_tooltip();
            if self.layer.take().is_some() {
                self.configured = false;
                self.requested = (0, 0);
                self.hovered = None;
                self.pressed = None;
            }
            return;
        }

        // One slot for the menu and the tooltip, since they are never on screen together; the
        // menu wins if both are somehow set, which is what the painter does too.
        let popup = self
            .menu
            .as_ref()
            .map(|posted| posted.size())
            .or_else(|| self.tooltip.as_ref().map(|tip| tip.size()));
        self.frame = Frame::new(
            self.shown.len(),
            self.anchor,
            self.orientation,
            self.metrics,
            popup,
            self.menu_at,
            self.output_size,
        );

        self.ensure_surface(qh);
        let wanted = (self.frame.width as u32, self.frame.height as u32);
        if wanted != self.requested
            && let Some(layer) = self.layer.as_ref()
        {
            layer.set_size(wanted.0, wanted.1);
            layer.commit();
            self.requested = wanted;
            // No paint yet: the size that will be drawn at is whatever the configure says, and
            // attaching a buffer of the old size to a surface being resized is how a tray ends up
            // with its strip clipped.
            return;
        }
        self.dirty = true;
    }

    /// The output the tray goes on: the configured one, else the leftmost.
    fn pick_output(&self, going: Option<&WlOutput>) -> Option<WlOutput> {
        let outputs: Vec<WlOutput> = self
            .output_state
            .outputs()
            .filter(|output| Some(output) != going)
            .collect();
        if let Some(wanted) = self.config.output.as_deref() {
            let named = outputs.iter().find(|output| {
                self.output_state
                    .info(output)
                    .and_then(|info| info.name)
                    .is_some_and(|name| name == wanted)
            });
            if let Some(output) = named {
                return Some(output.clone());
            }
            // Named but absent: say so once rather than silently landing somewhere else.
            eprintln!("wlrix-tray: no output named {wanted:?}; using the default");
        }
        outputs
            .iter()
            .find(|output| {
                self.output_state
                    .info(output)
                    .and_then(|info| info.logical_position)
                    .is_some_and(|(x, y)| x == 0 && y == 0)
            })
            .or_else(|| outputs.first())
            .cloned()
    }

    /// Create the strip surface, once there is an output to put it on.
    fn ensure_surface(&mut self, qh: &QueueHandle<Self>) {
        if self.layer.is_some() {
            return;
        }
        let Some(output) = self.pick_output(None) else {
            return;
        };
        self.output_size = self
            .output_state
            .info(&output)
            .and_then(|info| info.logical_size);

        let surface = self.compositor.create_surface(qh);
        let layer = self.layer_shell.create_layer_surface(
            qh,
            surface,
            // `Bottom`, not `Top`: IRIX's tray sat on the desktop rather than over the windows.
            // It is above `wlrix-bg` on the background layer, and -- because it maps later --
            // above `wlrix-desktop`, which shares this layer.
            Layer::Bottom,
            Some("wlrix-tray"),
            Some(&output),
        );
        layer.set_anchor(shell_anchor(self.anchor));
        // Zero, not a reserved strip: the tray is a desktop object, so it does not shrink anyone's
        // work area -- not the windows', not the desktop icons', and not the compositor's
        // minimized-window grid.
        layer.set_exclusive_zone(0);
        // Escape closes a menu, which needs the keyboard; `OnDemand` means the compositor hands it
        // over when the tray is clicked, and not before.
        layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
        set_margin(&layer, self.anchor, self.metrics.margin);
        let (width, height) = (self.frame.width as u32, self.frame.height as u32);
        layer.set_size(width, height);
        layer.commit();

        self.layer = Some(layer);
        self.output = Some(output);
        self.requested = (width, height);
        // Nothing may be attached until the compositor answers that commit.
        self.configured = false;
    }

    /// Re-read `tray.toml` and apply it.
    fn reload_config(&mut self, qh: &QueueHandle<Self>) {
        self.config = Config::load();
        self.metrics = self.config.metrics.resolve();
        let (palette, unknown) =
            wlrix_ui::palette::resolve(self.config.appearance.palette.as_deref());
        if let Some(wlrix_ui::palette::UnknownPalette(name)) = unknown {
            eprintln!(
                "wlrix-tray: no color scheme called {name:?}; using {}",
                palette.id
            );
        }
        self.palette = palette;

        // The corner and the direction are set on the surface itself, so a change to either needs
        // a new one -- `set_anchor` on a mapped surface is legal but the compositor is not
        // required to have moved it by the time the next buffer is attached.
        let moved = self.anchor != self.config.anchor;
        self.anchor = self.config.anchor;
        self.orientation = self.config.orientation;
        if moved && self.layer.take().is_some() {
            self.configured = false;
            self.requested = (0, 0);
        }

        // The icon size or the theme may have changed, and either invalidates every decoded
        // image. `set_theme` clears on a change of its own; the unconditional clear covers the
        // size.
        self.icons.set_theme(self.config.appearance.icon_theme());
        self.icons.clear();
        self.close_menu();
        self.clear_tooltip();
        let known: Vec<ItemAddress> = self.artwork.keys().cloned().collect();
        for address in known {
            self.render(&address);
        }
        self.refresh(qh);
    }

    /// Whether a tooltip is due but not yet shown, so the loop knows to wake up for it.
    fn tooltip_pending(&self) -> bool {
        self.hovered_since.is_some()
    }

    /// Show a tooltip once the pointer has rested long enough.
    ///
    /// Called from the loop rather than a timer source: pointer motion already wakes the loop, and
    /// what needs a wake-up is the pointer having *stopped*.
    fn tick_tooltip(&mut self, qh: &QueueHandle<Self>) {
        let Some(since) = self.hovered_since else {
            return;
        };
        if since.elapsed() < TOOLTIP_DELAY {
            return;
        }
        // Worked out once per hover, whether or not there turns out to be anything to show. An
        // item with an empty `ToolTip` -- which is most of them -- must not have it recomputed
        // every hundred milliseconds for as long as the pointer sits there.
        self.hovered_since = None;

        let Some(cell) = self.hovered else {
            return;
        };
        // A menu takes the slot a tooltip would use, and an item explaining itself underneath an
        // open menu is not something to explain.
        if self.menu.is_some() {
            return;
        }
        let Some(tip) = self
            .hovered_item(cell)
            .map(|item| item.properties.tooltip.clone())
        else {
            return;
        };

        let Tray { fonts, .. } = self;
        let line = fonts.line_height(Face::Bold, crate::tooltip::LABEL_PX);
        let Some(tooltip) = Tooltip::new(&tip, line, &mut Measured { fonts }) else {
            return;
        };
        self.tooltip = Some(tooltip);
        self.menu_at = cell;
        self.refresh(qh);
    }

    /// The item behind a cell.
    fn hovered_item(&self, cell: usize) -> Option<&crate::model::Item> {
        self.items.get(self.shown.get(cell)?)
    }

    /// Take any tooltip off the screen, and stop one from being due.
    ///
    /// Returns whether anything was showing, so a caller can skip a needless relayout.
    fn clear_tooltip(&mut self) -> bool {
        self.hovered_since = None;
        self.tooltip.take().is_some()
    }

    /// Where a point is in the posted menu's own coordinates.
    fn in_menu(&self, x: i32, y: i32) -> Option<(i32, i32)> {
        let origin = self.frame.popup?;
        Some((x - origin.x, y - origin.y))
    }

    /// Track the pointer over an open menu. Returns whether the layout changed.
    fn hover_menu(&mut self, x: i32, y: i32) -> bool {
        let Some((local_x, local_y)) = self.in_menu(x, y) else {
            return false;
        };
        let Tray { menu, fonts, .. } = self;
        let Some(posted) = menu.as_mut() else {
            return false;
        };
        let mut measure = |label: &str| fonts.width(Face::Bold, crate::menu::LABEL_PX, label);
        posted.hover(local_x, local_y, &mut measure)
    }

    /// Act on a left click while a menu is open.
    fn choose(&mut self, qh: &QueueHandle<Self>) {
        let choice = self.menu.as_ref().and_then(|posted| posted.choice());
        let target = self
            .menu
            .as_ref()
            .map(|posted| posted.menu.path.clone())
            .zip(self.menu_owner.clone());
        // Either way the menu goes away: choosing a row and dismissing the menu are both ends of
        // it, and a menu that stayed up after a choice would be a menu the user has to close twice.
        self.close_menu();
        if let (Some(id), Some((path, address))) = (choice, target) {
            self.bus.send(Command::Choose(address, path, id));
        }
        self.refresh(qh);
    }

    /// A press or release landed on a cell.
    fn on_cell(&mut self, qh: &QueueHandle<Self>, cell: usize, invocation: Invocation) {
        let Some(address) = self.shown.get(cell).cloned() else {
            return;
        };
        // `ItemIsMenu` is an item saying it has no activate action at all, only a menu. Calling
        // `Activate` on one does nothing, and the click reads as broken.
        let is_menu = self
            .items
            .get(&address)
            .is_some_and(|item| item.properties.item_is_menu);
        if invocation == Invocation::Activate && is_menu {
            self.open_menu(qh, cell);
            return;
        }
        let (x, y) = self.screen_position(cell);
        self.bus.send(Command::Invoke(address, invocation, x, y));
    }

    /// Turn accumulated scrolling into whole notches, and send them on.
    fn on_scroll(&mut self, cell: usize, horizontal: f64, vertical: f64) {
        let Some(address) = self.shown.get(cell).cloned() else {
            return;
        };
        self.scrolled.0 += horizontal;
        self.scrolled.1 += vertical;
        for (accumulated, axis) in [
            (&mut self.scrolled.0, Axis::Horizontal),
            (&mut self.scrolled.1, Axis::Vertical),
        ] {
            let notches = (*accumulated / SCROLL_NOTCH) as i32;
            if notches == 0 {
                continue;
            }
            *accumulated -= notches as f64 * SCROLL_NOTCH;
            self.bus
                .send(Command::Scroll(address.clone(), notches, axis));
        }
    }

    fn on_key(&mut self, qh: &QueueHandle<Self>, event: KeyEvent) {
        if event.keysym == Keysym::Escape && self.menu.is_some() {
            self.close_menu();
            self.refresh(qh);
        }
    }

    fn draw_if_dirty(&mut self) {
        if !self.dirty {
            return;
        }
        // There is nothing to attach to until the compositor has configured the surface, and
        // attaching anyway is a protocol error that takes the whole connection down with it.
        // `dirty` deliberately stays set, so the paint happens from `configure` instead of being
        // dropped on the floor.
        if !self.configured || self.layer.is_none() {
            return;
        }
        self.dirty = false;
        self.draw();
    }

    fn draw(&mut self) {
        // Destructured rather than reached through `self`, so the cells can borrow the items and
        // the artwork while the painter borrows the fonts and the pool.
        let Tray {
            layer,
            pool,
            fonts,
            palette,
            frame,
            menu,
            tooltip,
            items,
            artwork,
            shown,
            hovered,
            pressed,
            width,
            height,
            ..
        } = self;
        let Some(layer) = layer.as_ref() else { return };
        let (width, height) = ((*width).max(1) as i32, (*height).max(1) as i32);
        let stride = width * 4;

        let (buffer, pixels) =
            match pool.create_buffer(width, height, stride, wl_shm::Format::Argb8888) {
                Ok(pair) => pair,
                Err(err) => {
                    eprintln!("wlrix-tray: could not get a buffer to draw into: {err}");
                    return;
                }
            };

        let cells: Vec<paint::Cell> = shown
            .iter()
            .enumerate()
            .map(|(index, address)| {
                let art = artwork.get(address);
                paint::Cell {
                    icon: art.and_then(|art| art.icon.as_ref()),
                    overlay: art.and_then(|art| art.overlay.as_ref()),
                    state: if *pressed == Some(index) {
                        paint::CellState::Pressed
                    } else if *hovered == Some(index) {
                        paint::CellState::Hovered
                    } else {
                        paint::CellState::Idle
                    },
                    attention: items
                        .get(address)
                        .is_some_and(|item| item.properties.status == Status::NeedsAttention),
                }
            })
            .collect();

        let mut canvas = Canvas::new(pixels, width, height);
        paint::tray(
            &mut canvas,
            &mut paint::Scene {
                palette,
                fonts,
                frame,
                cells: &cells,
                menu: menu.as_ref(),
                tooltip: tooltip.as_ref(),
            },
        );

        let surface = layer.wl_surface();
        surface.damage_buffer(0, 0, width, height);
        if let Err(err) = buffer.attach_to(surface) {
            eprintln!("wlrix-tray: could not attach the buffer: {err}");
            return;
        }
        surface.commit();
    }
}

/// The font, as [`crate::tooltip`] wants to see it.
///
/// One borrow of `Fonts` behind two operations -- it measures mutably, caching its shaping -- so a
/// pair of closures could not both hold it.
struct Measured<'a> {
    fonts: &'a mut Fonts,
}

impl crate::tooltip::Text for Measured<'_> {
    fn width(&mut self, text: &str) -> i32 {
        self.fonts.width(Face::Bold, crate::tooltip::LABEL_PX, text)
    }

    fn wrap(&mut self, text: &str, width: i32) -> Vec<String> {
        wlrix_ui::text::wrap(
            self.fonts,
            Face::Regular,
            crate::tooltip::LABEL_PX,
            text,
            width,
            crate::tooltip::MAX_LINES,
        )
    }
}

/// The layer-shell anchor bits for a corner.
fn shell_anchor(anchor: Anchor) -> ShellAnchor {
    match anchor {
        Anchor::BottomLeft => ShellAnchor::BOTTOM | ShellAnchor::LEFT,
        Anchor::BottomRight => ShellAnchor::BOTTOM | ShellAnchor::RIGHT,
        Anchor::TopLeft => ShellAnchor::TOP | ShellAnchor::LEFT,
        Anchor::TopRight => ShellAnchor::TOP | ShellAnchor::RIGHT,
    }
}

/// Hold the strip `margin` away from the two edges it is anchored to.
///
/// The other two are left at zero: a margin on an edge the surface is not anchored to does
/// nothing, and setting one anyway would be a claim about the layout that is not true.
fn set_margin(layer: &LayerSurface, anchor: Anchor, margin: i32) {
    let (top, bottom) = match anchor {
        Anchor::TopLeft | Anchor::TopRight => (margin, 0),
        Anchor::BottomLeft | Anchor::BottomRight => (0, margin),
    };
    let (left, right) = match anchor {
        Anchor::TopLeft | Anchor::BottomLeft => (margin, 0),
        Anchor::TopRight | Anchor::BottomRight => (0, margin),
    };
    layer.set_margin(top, right, bottom, left);
}

impl CompositorHandler for Tray {
    fn scale_factor_changed(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _s: &WlSurface,
        _f: i32,
    ) {
    }
    fn transform_changed(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _s: &WlSurface,
        _t: wayland_client::protocol::wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _c: &Connection, _q: &QueueHandle<Self>, _s: &WlSurface, _t: u32) {}
    fn surface_enter(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _s: &WlSurface,
        _o: &WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _s: &WlSurface,
        _o: &WlOutput,
    ) {
    }
}

impl OutputHandler for Tray {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _c: &Connection, qh: &QueueHandle<Self>, _o: WlOutput) {
        // A tray started before the compositor finished enumerating monitors still gets a surface,
        // as soon as the first one shows up. This is also the way back from every monitor going at
        // once: a DisplayPort screen entering power save drops the link, so the compositor really
        // does destroy the outputs and advertise them again on wake.
        self.refresh(qh);
    }

    fn update_output(&mut self, _c: &Connection, _q: &QueueHandle<Self>, output: WlOutput) {
        if self.output.as_ref() != Some(&output) {
            return;
        }
        // A resolution change moves the far edge a menu has to stay inside of.
        self.output_size = self
            .output_state
            .info(&output)
            .and_then(|info| info.logical_size);
    }

    fn output_destroyed(&mut self, _c: &Connection, qh: &QueueHandle<Self>, output: WlOutput) {
        if self.output.as_ref() != Some(&output) {
            return;
        }
        // The monitor the tray was on has gone. Drop the surface and take the next one, so
        // unplugging a screen moves the tray rather than losing it. Leaving `layer` set to a
        // surface on the departed output would be the end of the tray: `ensure_surface` returns
        // early when there is one, so the monitors coming back would never build a new surface.
        self.forget_surface();
        self.refresh(qh);
    }
}

impl Tray {
    /// Throw away the surface, so the next [`Tray::refresh`] builds a fresh one.
    fn forget_surface(&mut self) {
        self.layer = None;
        self.output = None;
        self.output_size = None;
        self.configured = false;
        self.width = 0;
        self.height = 0;
        self.requested = (0, 0);
        self.hovered = None;
        self.pressed = None;
        self.close_menu();
        self.clear_tooltip();
    }
}

impl LayerShellHandler for Tray {
    fn closed(&mut self, _c: &Connection, qh: &QueueHandle<Self>, layer: &LayerSurface) {
        // Not the end of the tray. The compositor closes every layer surface on an output it is
        // removing, and an output being removed is what a DisplayPort monitor entering power save
        // looks like -- so exiting here would mean the tray never came back from an idle blank.
        if self.layer.as_ref().map(WaylandSurface::wl_surface) != Some(layer.wl_surface()) {
            // A surface already replaced, closed on its way out. Nothing to do: acting on it would
            // tear down the live one.
            return;
        }
        self.forget_surface();
        self.refresh(qh);
    }

    fn configure(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        // `sctk` acked this on the way in, so the surface is drawable from here on.
        self.configured = true;
        // A compositor that answers (0, 0) is saying "you choose", which is what was asked for.
        let (width, height) = match configure.new_size {
            (0, 0) => self.requested,
            size => size,
        };
        self.width = width;
        self.height = height;
        // Either way it still needs a paint: the surface has no content until one is attached, and
        // this is where a paint held back for want of a configure finally gets to run.
        self.dirty = true;
    }
}

impl SeatHandler for Tray {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _c: &Connection, _q: &QueueHandle<Self>, _seat: WlSeat) {}

    fn new_capability(
        &mut self,
        _c: &Connection,
        qh: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        match capability {
            Capability::Pointer if self.pointer.is_none() => {
                match self.seat_state.get_pointer(qh, &seat) {
                    Ok(pointer) => self.pointer = Some(pointer),
                    Err(err) => eprintln!("wlrix-tray: no pointer: {err}"),
                }
            }
            Capability::Keyboard if self.keyboard.is_none() => {
                match self.seat_state.get_keyboard(qh, &seat, None) {
                    Ok(keyboard) => self.keyboard = Some(keyboard),
                    // Only Escape depends on it, so this costs a shortcut rather than the tray.
                    Err(err) => {
                        eprintln!("wlrix-tray: no keyboard ({err}); Escape will not close a menu")
                    }
                }
            }
            _ => {}
        }
    }

    fn remove_capability(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _seat: WlSeat,
        capability: Capability,
    ) {
        match capability {
            Capability::Pointer => {
                if let Some(pointer) = self.pointer.take() {
                    pointer.release();
                }
            }
            Capability::Keyboard => {
                if let Some(keyboard) = self.keyboard.take() {
                    keyboard.release();
                }
            }
            _ => {}
        }
    }

    fn remove_seat(&mut self, _c: &Connection, _q: &QueueHandle<Self>, _seat: WlSeat) {}
}

impl KeyboardHandler for Tray {
    fn enter(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _k: &WlKeyboard,
        _s: &WlSurface,
        _serial: u32,
        _raw: &[u32],
        _keysyms: &[Keysym],
    ) {
    }

    fn leave(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _k: &WlKeyboard,
        _s: &WlSurface,
        _serial: u32,
    ) {
    }

    fn press_key(
        &mut self,
        _c: &Connection,
        qh: &QueueHandle<Self>,
        _k: &WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        self.on_key(qh, event);
    }

    fn release_key(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _k: &WlKeyboard,
        _serial: u32,
        _event: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _k: &WlKeyboard,
        _serial: u32,
        modifiers: Modifiers,
        _layout: u32,
    ) {
        self.modifiers = modifiers;
    }
}

impl PointerHandler for Tray {
    fn pointer_frame(
        &mut self,
        _c: &Connection,
        qh: &QueueHandle<Self>,
        _pointer: &WlPointer,
        events: &[PointerEvent],
    ) {
        let Some(surface) = self.layer.as_ref().map(|layer| layer.wl_surface().clone()) else {
            return;
        };

        for event in events {
            if event.surface != surface {
                continue;
            }
            let (x, y) = (event.position.0 as i32, event.position.1 as i32);
            match event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    // An open menu owns the pointer: it is drawn over the strip, so highlighting a
                    // cell underneath would light up something the user cannot reach without
                    // putting the menu away first.
                    if self.menu.is_some() {
                        if self.hover_menu(x, y) {
                            // A submenu may have opened, which changes how big the surface has to
                            // be -- so this goes through `refresh` rather than just repainting.
                            self.refresh(qh);
                        }
                        continue;
                    }
                    let hovered = self.frame.cell_at(x, y);
                    if hovered != self.hovered {
                        self.hovered = hovered;
                        self.dirty = true;
                        // Moving to another cell restarts the clock and takes down whatever was
                        // showing; a tooltip that stayed put while the pointer moved on would be
                        // describing the wrong item.
                        let had = self.clear_tooltip();
                        self.hovered_since = hovered.map(|_| std::time::Instant::now());
                        if had {
                            self.refresh(qh);
                        }
                    }
                }
                PointerEventKind::Leave { .. } => {
                    self.dirty |= self.hovered.is_some() || self.pressed.is_some();
                    self.hovered = None;
                    self.pressed = None;
                    if self.clear_tooltip() {
                        self.refresh(qh);
                        continue;
                    }
                    // The pointer has left the surface entirely, which includes the menu -- there
                    // is nowhere left for it to be pointing.
                    if self.menu.is_some() {
                        self.close_menu();
                        self.refresh(qh);
                    }
                }
                PointerEventKind::Press { button, .. } if button == BTN_LEFT => {
                    // With a menu open, a press either chooses a row or puts the menu away. Either
                    // way it does not reach the strip underneath.
                    if self.menu.is_some() {
                        self.choose(qh);
                        continue;
                    }
                    self.pressed = self.frame.cell_at(x, y);
                    self.dirty |= self.pressed.is_some();
                    if self.clear_tooltip() {
                        self.refresh(qh);
                    }
                }
                PointerEventKind::Release { button, .. } if button == BTN_LEFT => {
                    let pressed = self.pressed.take();
                    self.dirty |= pressed.is_some();
                    // Only a press and release on the *same* cell activates, so dragging off a
                    // cell before letting go cancels -- as a button does.
                    if let Some(cell) = pressed
                        && self.frame.cell_at(x, y) == Some(cell)
                    {
                        self.on_cell(qh, cell, Invocation::Activate);
                    }
                }
                PointerEventKind::Press { button, .. } if button == BTN_RIGHT => {
                    if self.menu.is_some() {
                        self.close_menu();
                        self.refresh(qh);
                        continue;
                    }
                    if let Some(cell) = self.frame.cell_at(x, y) {
                        self.open_menu(qh, cell);
                    }
                }
                PointerEventKind::Release { button, .. } if button == BTN_MIDDLE => {
                    if self.menu.is_none()
                        && let Some(cell) = self.frame.cell_at(x, y)
                    {
                        self.on_cell(qh, cell, Invocation::Secondary);
                    }
                }
                PointerEventKind::Axis {
                    horizontal,
                    vertical,
                    ..
                } => {
                    if self.menu.is_some() {
                        continue;
                    }
                    if let Some(cell) = self.frame.cell_at(x, y) {
                        // A wheel reports discrete notches and a touchpad reports a stream of
                        // fractions. Taking the discrete count when there is one keeps a wheel
                        // click exactly one notch, whatever the compositor scales it by.
                        let (h, v) = match (horizontal.discrete, vertical.discrete) {
                            (0, 0) => (horizontal.absolute, vertical.absolute),
                            (h, v) => (h as f64 * SCROLL_NOTCH, v as f64 * SCROLL_NOTCH),
                        };
                        self.on_scroll(cell, h, v);
                    }
                }
                _ => {}
            }
        }
    }
}

impl ShmHandler for Tray {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Tray {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(Tray);
delegate_output!(Tray);
delegate_shm!(Tray);
delegate_seat!(Tray);
delegate_pointer!(Tray);
delegate_keyboard!(Tray);
delegate_layer!(Tray);
delegate_registry!(Tray);
