# wlrix-tray

The application indicator tray for the wlRIX desktop. A small dock in the corner of the screen holding the status icons
background programs publish — fcitx5's input-method indicator, Steam, and anything else that speaks StatusNotifierItem —
drawn in 4Dwm chrome.

- **Language:** Rust
- **License:** GPL-3.0-or-later

IRIX had one of these, most often seen holding the wnn input-method indicator, docked in the bottom-left corner of the
desktop. This is that, for the programs a Linux desktop actually runs.

## What it speaks

**`org.kde.StatusNotifierItem` only.** No XEmbed, no `_NET_SYSTEM_TRAY_S0`: that is an X11 protocol needing compositor
surface embedding that wlRIX does not have, and everything worth hosting has spoken SNI for a decade. fcitx5 ships
`libnotificationitem.so` and Steam goes through libappindicator; both land here.

This program **is** `org.kde.StatusNotifierWatcher`. That is not an implementation detail — until something owns that
name, applications publish no item at all, so a tray that is not running is an fcitx5 with no indicator. It also owns
`org.kde.StatusNotifierHost-<pid>` and answers `IsStatusNotifierHostRegistered` with `true`, because several toolkits
wait for a host before publishing anything.

An item's menu is a full **`com.canonical.dbusmenu`** client, drawn as a 4Dwm menu. That is the half that makes the tray
useful rather than decorative: fcitx5's input-method list lives there and nowhere else, and an item's own `ContextMenu`
method — what a tray calls when it cannot speak dbusmenu — is answered by doing nothing on Wayland by almost every
application there is.

## Where it sits, and why that is load-bearing

A **wlr-layer-shell bottom surface**, anchored to a corner, with **no exclusive zone**. Windows cover it, and nobody's
work area shrinks because a program started publishing a status icon — it is a desktop object, the way IRIX's was, not a
panel.

Two consequences worth knowing before changing anything here:

- **The surface is exactly the size of what it draws.** `wlrix-desktop` is a *fullscreen* bottom-layer surface, and the
  compositor's `layer_under` picks the topmost bottom-layer surface by bounding box; when its input region rejects the
  point it falls through to the **background** layer, not to the desktop underneath. A transparent margin around the
  tray would therefore swallow clicks that belong to the desktop icons.
- **It must start after `wlrix-desktop`.** Both are on the bottom layer, and the one that maps later sorts above.
  `wlrix-session`'s `DEFAULT_APPS` puts it there.

### Menus are drawn in this surface, not in an `xdg_popup`

`wlrix-compositor` never unconstrains a popup whose root is a layer surface — it looks the root up in its window space
and returns early — so a tray popup would not be kept on screen; and popup grabs are a standing TODO there, so
`xdg_popup.grab()` would not dismiss on an outside click. So the surface grows to hold the menu, which needs no
compositor change and is what `wlrix-desktop` already does for its own context menu.

The cost, and it is a deliberate one: while a menu is open the surface covers the corner between the menu and the strip,
and a click there dismisses the menu rather than reaching the desktop. That is what a menu grab does everywhere else.

## Using it

|              |                                                              |
|--------------|--------------------------------------------------------------|
| left click   | `Activate` — or opens the menu, if the item set `ItemIsMenu` |
| middle click | `SecondaryActivate`                                          |
| right click  | opens the item's menu                                        |
| scroll       | `Scroll` — fcitx5 cycles input methods with it               |
| `Esc`        | closes an open menu                                          |
| hover        | shows the item's `ToolTip` after a moment                    |

The tooltip is not decoration either. An input-method indicator draws one keyboard whatever method is active — fcitx5
publishes `IconName = "input-keyboard-symbolic"` for both Mozc and plain Japanese keyboard input — so the icon cannot
say which is on and the `ToolTip` is the only thing that can.

Items whose `Status` is `Passive` are hidden by default; the specification says a tray may, and applications rely on it,
so showing them all turns the strip into a list of everything that has ever started. An item asking for
`NeedsAttention` gets its attention icon and a filled cell.

## Configuration

`~/.config/wlrix/tray.toml`, then `/etc/wlrix/tray.toml`. The first file found wins outright rather than merging, so
what you see in your own file is the whole of what you get — the same shape as every other wlRIX component. Unknown keys
are refused, and `wlrix-tray --check-config <path>` says whether a file would be accepted.

```toml
output = "DP-1"              # which monitor; default is the leftmost
anchor = "bottom-left"       # bottom-left | bottom-right | top-left | top-right
orientation = "horizontal"   # horizontal | vertical
show_passive = false         # items whose Status is Passive are hidden
hide_when_empty = true       # with nothing to show, show nothing at all

[appearance]
palette = "gotham"           # the color scheme; default is "classic"
icon_theme = "Adwaita"       # where an item's IconName is looked up; "" for none

[metrics]
icon = 22                    # the icon artwork, square
cell = 28                    # one cell
gap = 2                      # between cells
margin = 8                   # between the strip and the screen edges
wrap_at = 8                  # cells per run before the strip wraps

[[item]]                     # optional per-item overrides, keyed by the item's D-Bus Id
id = "Fcitx"
order = 0
hidden = false
```

`SIGHUP` re-reads the file, so `wlrix-settings-daemon` can apply a change live; the pidfile is
`$XDG_RUNTIME_DIR/wlrix-tray.pid`.

### `icon_theme` earns its default

fcitx5 publishes `IconName = "input-keyboard-symbolic"` with no pixmap and no `IconThemePath`. That file is in Adwaita —
and in every other icon theme — and in **hicolor** on none of them, and hicolor plus `/usr/share/pixmaps` is all a
themeless lookup searches. So without a named theme the commonest item there is draws as a placeholder. Adwaita is what
a GTK application would pick and therefore what every other tray on the machine is already showing.

### `[[item]]` is not a settings-daemon key

It is an array of tables — a keyed collection needing add/remove/reorder rather than get/set — and
`wlrix-settings-daemon` deliberately manages only scalar leaves. The keys above it are declared in the daemon's schema
and can be set over D-Bus; the item overrides stay hand-edited.

## Testing it

The parts that can be tested without a bus or a screen are, and they are most of the interesting ones: the strip layout
for four corners and two orientations, the menu cascade and tooltip geometry, the `IconPixmap` conversion (big-endian,
not premultiplied — both easy to get wrong and neither obvious on screen), the dbusmenu parser against fcitx5's actual
`GetLayout` reply, and item ordering and filtering.

```console
$ cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all --check
```

The bus and the surface need a session. `examples/fake_item.rs` registers synthetic items so the tray can be driven with
no fcitx5 and no Steam:

```console
$ cargo run --example fake_item -- --count 3 --attention
```

The first one registers **KDE-style**, by bus name at `/StatusNotifierItem`; the rest register **Ayatana-style**, by
object path. Both forms are in there because reading the second as a bus name is the classic way to host every tray icon
except the libappindicator ones. `kill -9` on it tests the removal path that actually matters — a crashed application
says nothing on the way out, and `NameOwnerChanged` is the only notice a tray gets.

```console
$ busctl --user introspect org.kde.StatusNotifierWatcher /StatusNotifierWatcher
```

## Threads

Three, and the shape is `wlrix-idle`'s and the portal's: the bus reports into the main loop through a
`calloop::channel`, the loop owns every piece of state, and no async runtime touches it.

| thread     | what it does                                                                                                          |
|------------|-----------------------------------------------------------------------------------------------------------------------|
| zbus's own | dispatches method calls to the watcher object; registrations go straight down the channel                             |
| signals    | one `MessageIterator` with four match rules — item changes, `PropertiesChanged`, `NameOwnerChanged`, dbusmenu updates |
| items      | every read from and call to an item                                                                                   |

That last one is the only place this differs from `xdg-desktop-portal-wlrix`, which makes its outgoing calls from the
loop. It talks to processes it trusts to answer; an item is a Steam that may be swapping, and a blocking `Activate`
against it would stall the Wayland connection — the tray would stop drawing because something else stopped answering.

Every change signal is treated as "read it all again". `NewIcon` and friends carry no arguments, some items emit only a
subset of them, and Ayatana-derived items emit a plain `PropertiesChanged` instead — so five nearly identical paths and
a sixth for the items that use none of them collapse into one `GetAll`.
