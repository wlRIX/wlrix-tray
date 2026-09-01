// SPDX-License-Identifier: GPL-3.0-or-later
//! StatusNotifierItem, on threads of its own.
//!
//! This program *is* `org.kde.StatusNotifierWatcher`. Without something owning that name, fcitx5
//! and Steam publish no item at all -- so the bus side is not an accessory to the window, it is
//! the reason the window has anything in it.
//!
//! calloop is resolutely synchronous, so the two are kept apart rather than reconciled, the shape
//! `wlrix-idle` set and `xdg-desktop-portal-wlrix` follows: the bus reports into the main loop
//! through a `calloop::channel`, the loop owns every piece of state, and no async runtime ever
//! touches it.
//!
//! # Three threads, and why it is not one
//!
//! - **zbus's own**, dispatching method calls to the [`watcher`] object. Registrations arrive here
//!   and go straight down the channel.
//! - **The signal thread** ([`worker::start`]), one [`zbus::blocking::MessageIterator`] over every
//!   signal the tray subscribed to. One iterator rather than a proxy per item: an item may come
//!   and go faster than the loop notices, and a match rule that names an *interface* cannot go
//!   stale the way a proxy bound to a departed name does.
//! - **The worker thread** ([`worker::start_worker`]), which does every read from and call to an
//!   item.
//!
//! That last one is the only place this differs from the portal, and deliberately. The portal
//! makes its outgoing calls from the loop, because they go to processes it trusts to answer. An
//! item is a Steam that may be swapping, and a blocking `Activate` against it would stall the
//! Wayland connection -- the tray would stop drawing because something else stopped answering.
//! Commands go to the worker by [`std::sync::mpsc`] and answers come back by the same calloop
//! channel as everything else.

pub mod dbusmenu;
pub mod item;
pub mod watcher;
pub mod worker;

use std::sync::{Arc, Mutex};

pub use item::{ItemProperties, Status};

/// The name every tray owns and every item looks for.
pub const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
/// Where the watcher interface is served. Fixed by the specification, not a choice.
pub const WATCHER_PATH: &str = "/StatusNotifierWatcher";
/// The interface an item serves.
pub const ITEM_INTERFACE: &str = "org.kde.StatusNotifierItem";
/// Where a KDE-style item serves it when it registered by bus name.
pub const DEFAULT_ITEM_PATH: &str = "/StatusNotifierItem";
/// The menu interface an item points at with its `Menu` property.
pub const MENU_INTERFACE: &str = "com.canonical.dbusmenu";

/// One item, as somewhere to send a message.
///
/// `service` is always a *unique* name (`:1.42`), never a well-known one: the sender of the
/// registration is what the tray talks to, so an item that later loses a well-known name is still
/// reachable, and two items that briefly share one cannot be confused.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ItemAddress {
    pub service: String,
    pub path: String,
}

impl ItemAddress {
    /// Work out where an item is from what it passed to `RegisterStatusNotifierItem` and who sent
    /// it.
    ///
    /// The argument is **either** a bus name **or** an object path, and which one it is decides
    /// what the other half is. KDE-style clients (fcitx5) pass a name and serve at
    /// [`DEFAULT_ITEM_PATH`]; libappindicator clients pass `/org/ayatana/NotificationItem/<id>`
    /// and mean "the sender's own name, at this path". Reading the second kind as a bus name is
    /// the classic way to host every tray icon except the Ayatana ones.
    pub fn resolve(argument: &str, sender: &str) -> Self {
        if argument.starts_with('/') {
            return Self {
                service: sender.to_owned(),
                path: argument.to_owned(),
            };
        }
        Self {
            // The argument may be a well-known name the item also owns. The sender is what is
            // answered to, so it wins -- and it is what `NameOwnerChanged` will name on the way
            // out.
            service: sender.to_owned(),
            path: DEFAULT_ITEM_PATH.to_owned(),
        }
    }
}

/// What the bus tells the main loop.
#[derive(Debug)]
pub enum TrayEvent {
    /// An item registered. Its properties have not been read yet.
    Registered(ItemAddress),
    /// An item's process went away, or it asked to be removed.
    Gone(ItemAddress),
    /// A fresh read of everything an item publishes.
    Properties(ItemAddress, Box<ItemProperties>),
    /// An item said something about it changed; the loop should ask for a re-read.
    Changed(ItemAddress),
    /// A menu, ready to post. `Err` carries why there is none, for the log.
    Menu(ItemAddress, Result<dbusmenu::Menu, String>),
    /// An open menu's contents changed underneath it.
    MenuStale(ItemAddress),
}

/// What the main loop asks the bus to do.
#[derive(Debug)]
pub enum Command {
    /// Read every property of an item.
    Read(ItemAddress),
    /// `Activate`, `SecondaryActivate` or `ContextMenu`, at a screen position.
    Invoke(ItemAddress, Invocation, i32, i32),
    /// `Scroll`, by a delta along an axis.
    Scroll(ItemAddress, i32, Axis),
    /// `AboutToShow` then `GetLayout`, answered with [`TrayEvent::Menu`].
    OpenMenu(ItemAddress, String),
    /// A menu item was chosen: `Event(id, "clicked", …)`.
    Choose(ItemAddress, String, i32),
    /// Stop.
    Stop,
}

/// Which of the three activation methods to call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invocation {
    Activate,
    Secondary,
    ContextMenu,
}

impl Invocation {
    pub(crate) fn method(self) -> &'static str {
        match self {
            Invocation::Activate => "Activate",
            Invocation::Secondary => "SecondaryActivate",
            Invocation::ContextMenu => "ContextMenu",
        }
    }
}

/// Which way a scroll went, in the spelling the specification uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Vertical,
    Horizontal,
}

impl Axis {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Axis::Vertical => "vertical",
            Axis::Horizontal => "horizontal",
        }
    }
}

/// The registered items, shared between the watcher object and the signal thread.
///
/// The signal thread needs this because a crashed item emits nothing: `NameOwnerChanged` names a
/// bus that went away, and only this list says which item that was. The specification's own
/// `StatusNotifierItemUnregistered` covers the polite case only, which is the rare one.
#[derive(Default)]
pub struct Registry {
    items: Mutex<Vec<ItemAddress>>,
}

impl Registry {
    /// Record an item. `false` when it was already there, which happens when an application
    /// registers again after seeing the watcher reappear.
    pub fn add(&self, address: ItemAddress) -> bool {
        let mut items = self.items.lock().unwrap_or_else(|err| err.into_inner());
        if items.contains(&address) {
            return false;
        }
        items.push(address);
        true
    }

    /// Forget every item served by `service`, and say which they were.
    pub fn remove_service(&self, service: &str) -> Vec<ItemAddress> {
        let mut items = self.items.lock().unwrap_or_else(|err| err.into_inner());
        let (gone, kept): (Vec<_>, Vec<_>) =
            items.drain(..).partition(|item| item.service == service);
        *items = kept;
        gone
    }

    /// Every item, as the `RegisteredStatusNotifierItems` property spells them.
    pub fn listed(&self) -> Vec<String> {
        self.items
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .iter()
            .map(|item| format!("{}{}", item.service, item.path))
            .collect()
    }

    /// Every item `service` serves.
    ///
    /// One process may serve several -- fcitx5 does, briefly, when it restarts its notification
    /// item -- so a signal from one bus name may be about more than one cell.
    pub fn for_service(&self, service: &str) -> Vec<ItemAddress> {
        self.items
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .iter()
            .filter(|item| item.service == service)
            .cloned()
            .collect()
    }

    /// Whether `service` serves any item, so a caller can ignore the rest of the bus.
    pub fn knows(&self, service: &str) -> bool {
        !self.for_service(service).is_empty()
    }
}

/// The bus, once it is up: what the main loop keeps hold of.
pub struct Bus {
    /// Commands to the worker thread. Sending on a closed channel is ignored -- the worker only
    /// closes when the tray is on its way out.
    commands: std::sync::mpsc::Sender<Command>,
}

impl Bus {
    /// Ask the worker to do something. Never blocks and never fails: a command that cannot be
    /// delivered is one the tray no longer needs delivered.
    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
    }
}

/// Own the watcher name, announce a host, and start the two threads.
///
/// Fails rather than carrying on if the name cannot be had. A tray that is running but is not the
/// watcher is worse than one that is not running: every item would register with whoever *is*, and
/// the user would be looking at an empty strip with no way to tell why.
pub fn spawn(replace: bool) -> Result<(Bus, calloop::channel::Channel<TrayEvent>), String> {
    let (sender, channel) = calloop::channel::channel();
    let registry = Arc::new(Registry::default());

    let connection = zbus::blocking::connection::Builder::session()
        .map_err(|err| format!("no session bus: {err}"))?
        // The object is served before the name is requested, so an application that registers the
        // instant the name appears never finds an empty connection behind it. That is also why the
        // name is not requested through the builder: it would be taken before this point, leaving
        // a window in which the interface is not there yet.
        .serve_at(
            WATCHER_PATH,
            watcher::Watcher {
                registry: Arc::clone(&registry),
                sender: sender.clone(),
            },
        )
        .map_err(|err| format!("could not serve the watcher interface: {err}"))?
        .build()
        .map_err(|err| format!("could not connect to the session bus: {err}"))?;

    // `DoNotQueue`: queueing would leave this process running and reachable at its unique name but
    // not at the well-known one, so items would keep registering with whoever holds it while this
    // sat waiting for a turn that may never come.
    //
    // `AllowReplacement` **always**, and it is not optional the way it looks. D-Bus replacement is
    // granted by the *incumbent*, not taken by the newcomer: a name requested without this flag
    // can never be replaced, so `--replace` on a later run fails with "name already taken" no
    // matter what that run asks for. Setting it only when `--replace` was passed -- the obvious
    // reading -- makes the flag protect the wrong process and never work.
    let mut flags =
        zbus::fdo::RequestNameFlags::DoNotQueue | zbus::fdo::RequestNameFlags::AllowReplacement;
    if replace {
        flags |= zbus::fdo::RequestNameFlags::ReplaceExisting;
    }
    let reply = connection
        .request_name_with_flags(WATCHER_NAME, flags)
        .map_err(|err| format!("could not request {WATCHER_NAME}: {err}"))?;
    if !matches!(
        reply,
        zbus::fdo::RequestNameReply::PrimaryOwner | zbus::fdo::RequestNameReply::AlreadyOwner
    ) {
        return Err(format!(
            "{WATCHER_NAME} is already owned by another tray ({reply:?}); pass --replace to take \
             it. If --replace was passed and this still failed, the process holding the name \
             predates AllowReplacement and has to be stopped by hand."
        ));
    }

    // A *host* is the thing that displays items, and several toolkits publish nothing until one
    // exists -- they take `IsStatusNotifierHostRegistered` at its word. The name is per-process by
    // convention, so it never collides with another tray's.
    let host = format!("org.kde.StatusNotifierHost-{}", std::process::id());
    if let Err(err) = connection.request_name_with_flags(
        host.as_str(),
        zbus::fdo::RequestNameFlags::DoNotQueue | zbus::fdo::RequestNameFlags::AllowReplacement,
    ) {
        // Not fatal. KDE-style items register regardless, and losing the Ayatana ones is a smaller
        // failure than refusing to start a tray at all.
        eprintln!("wlrix-tray: could not take {host}: {err}");
    }
    watcher::announce_host(&connection, &host);

    worker::start(&connection, Arc::clone(&registry), sender.clone());
    let commands = worker::start_worker(&connection, sender);

    Ok((Bus { commands }, channel))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kde_item_registers_by_name_and_serves_at_the_default_path() {
        let address = ItemAddress::resolve("org.kde.StatusNotifierItem-37048-1", ":1.76");
        assert_eq!(address.service, ":1.76");
        assert_eq!(address.path, DEFAULT_ITEM_PATH);
    }

    #[test]
    fn an_ayatana_item_registers_by_path() {
        // Read as a bus name this would be an unroutable destination, and every libappindicator
        // icon -- Steam's included -- would silently never appear.
        let address = ItemAddress::resolve("/org/ayatana/NotificationItem/steam", ":1.99");
        assert_eq!(address.service, ":1.99");
        assert_eq!(address.path, "/org/ayatana/NotificationItem/steam");
    }

    #[test]
    fn the_sender_is_always_what_gets_talked_to() {
        // An item may pass a well-known name it also owns. Answering to that rather than to the
        // sender would break as soon as it dropped the name, and would confuse two items that
        // took turns holding it.
        let address = ItemAddress::resolve("org.example.Well.Known", ":1.5");
        assert_eq!(address.service, ":1.5");
    }

    #[test]
    fn a_registry_forgets_everything_one_process_served() {
        let registry = Registry::default();
        assert!(registry.add(ItemAddress::resolve("/one", ":1.1")));
        assert!(registry.add(ItemAddress::resolve("/two", ":1.1")));
        assert!(registry.add(ItemAddress::resolve("/three", ":1.2")));
        // Registering twice is not an error: an application does it again when it sees the watcher
        // reappear, and a second cell for the same item would be a bug the user can see.
        assert!(!registry.add(ItemAddress::resolve("/one", ":1.1")));

        assert!(registry.knows(":1.1"));
        let gone = registry.remove_service(":1.1");
        assert_eq!(gone.len(), 2, "one process may serve several items");
        assert!(!registry.knows(":1.1"));
        assert_eq!(registry.listed(), vec![":1.2/three".to_string()]);
    }
}
