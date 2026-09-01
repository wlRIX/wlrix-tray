// SPDX-License-Identifier: GPL-3.0-or-later
//! A synthetic StatusNotifierItem, for exercising the tray without fcitx5 or Steam.
//!
//! Registers one or more items with whatever owns `org.kde.StatusNotifierWatcher`, each with a
//! generated icon and a small `com.canonical.dbusmenu` menu, and prints what the tray asks of
//! them. Killing it is how the removal path gets tested; `-9` in particular, because a process
//! killed outright says nothing on the way out and the tray has to notice through
//! `NameOwnerChanged` alone.
//!
//! ```console
//! $ cargo run --example fake_item -- --count 3 --attention
//! ```
//!
//! The first item registers **KDE-style** -- by bus name, serving at `/StatusNotifierItem` -- and
//! the rest register **Ayatana-style**, by object path. Both forms are in here because reading the
//! second as a bus name is the classic way to host every tray icon except the libappindicator
//! ones, and a test rig that only exercises one form would never catch it.

use std::collections::HashMap;

use zbus::zvariant::{OwnedValue, Signature, StructureBuilder, Value};

/// The colors the generated icons cycle through, so several items are told apart at a glance.
const COLORS: [(u8, u8, u8); 4] = [
    (0x4a, 0x90, 0xd9),
    (0xd9, 0x7a, 0x4a),
    (0x6a, 0xb0, 0x5a),
    (0xb0, 0x5a, 0xa0),
];

/// `a(iiay)`: the pixmap array both `IconPixmap` and the tooltip's icon field are.
type Pixmaps = Vec<(i32, i32, Vec<u8>)>;

/// `(sa(iiay)ss)`: the `ToolTip` property.
type ToolTip = (String, Pixmaps, String, String);

/// One synthetic item.
struct FakeItem {
    id: String,
    title: String,
    status: String,
    pixmap: Pixmaps,
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl FakeItem {
    fn activate(&self, x: i32, y: i32) {
        println!("{}: Activate at ({x}, {y})", self.id);
    }

    fn secondary_activate(&self, x: i32, y: i32) {
        println!("{}: SecondaryActivate at ({x}, {y})", self.id);
    }

    fn context_menu(&self, x: i32, y: i32) {
        println!("{}: ContextMenu at ({x}, {y})", self.id);
    }

    fn scroll(&self, delta: i32, orientation: String) {
        println!("{}: Scroll {delta} {orientation}", self.id);
    }

    #[zbus(property)]
    fn id(&self) -> String {
        self.id.clone()
    }

    #[zbus(property)]
    fn title(&self) -> String {
        self.title.clone()
    }

    #[zbus(property)]
    fn status(&self) -> String {
        self.status.clone()
    }

    #[zbus(property)]
    fn category(&self) -> String {
        "ApplicationStatus".to_owned()
    }

    /// Deliberately empty, so the pixmap path is what gets exercised. A name here would resolve
    /// against the installed themes and the raw-ARGB decoding would never run.
    #[zbus(property)]
    fn icon_name(&self) -> String {
        String::new()
    }

    #[zbus(property)]
    fn icon_pixmap(&self) -> Pixmaps {
        self.pixmap.clone()
    }

    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn menu(&self) -> zbus::zvariant::ObjectPath<'static> {
        zbus::zvariant::ObjectPath::from_static_str_unchecked("/MenuBar")
    }

    #[zbus(property)]
    fn tool_tip(&self) -> ToolTip {
        (
            String::new(),
            Vec::new(),
            self.title.clone(),
            format!("a synthetic item called {}", self.id),
        )
    }
}

/// The menu behind the items: a submenu, a separator, and two plain rows.
///
/// One menu object shared by every item, which is fine -- they are all the same fake program --
/// and it keeps the interesting part, the layout, in one place.
struct FakeMenu;

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl FakeMenu {
    /// The tray calls this before showing a menu. Answering `true` means "the layout changed",
    /// which is what a real application that builds its menu here would say.
    fn about_to_show(&self, id: i32) -> bool {
        println!("menu: AboutToShow({id})");
        true
    }

    fn event(&self, id: i32, event_id: String, _data: Value<'_>, _timestamp: u32) {
        println!("menu: Event({id}, {event_id})");
    }

    #[allow(clippy::type_complexity)]
    fn get_layout(
        &self,
        parent_id: i32,
        _recursion_depth: i32,
        _property_names: Vec<String>,
    ) -> (u32, (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>)) {
        println!("menu: GetLayout({parent_id})");
        // Only the root is served: the submenu's children come back inside it, which is what
        // `recursionDepth = -1` asks for and what the tray sends.
        let children = if parent_id == 0 {
            vec![
                row(
                    1,
                    "_Input Method",
                    Some("submenu"),
                    None,
                    vec![
                        row(10, "Mozc", None, Some(("radio", 1)), Vec::new()),
                        row(11, "Keyboard", None, Some(("radio", 0)), Vec::new()),
                    ],
                ),
                separator(2),
                row(
                    3,
                    "Show Something",
                    None,
                    Some(("checkmark", 1)),
                    Vec::new(),
                ),
                disabled_row(4, "Busy"),
                row(5, "E_xit", None, None, Vec::new()),
            ]
        } else {
            Vec::new()
        };
        (1, (parent_id, HashMap::new(), children))
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }

    #[zbus(property)]
    fn status(&self) -> String {
        "normal".to_owned()
    }
}

/// One `(ia{sv}av)` row.
fn row(
    id: i32,
    label: &str,
    children_display: Option<&str>,
    toggle: Option<(&str, i32)>,
    children: Vec<OwnedValue>,
) -> OwnedValue {
    let mut properties = properties();
    put(&mut properties, "label", Value::from(label.to_owned()));
    if let Some(display) = children_display {
        put(
            &mut properties,
            "children-display",
            Value::from(display.to_owned()),
        );
    }
    if let Some((kind, state)) = toggle {
        put(&mut properties, "toggle-type", Value::from(kind.to_owned()));
        put(&mut properties, "toggle-state", Value::from(state));
    }
    structure(id, properties, children)
}

fn separator(id: i32) -> OwnedValue {
    let mut properties = properties();
    put(&mut properties, "type", Value::from("separator".to_owned()));
    structure(id, properties, Vec::new())
}

/// A row that is there but grayed out, so the disabled path gets drawn too.
fn disabled_row(id: i32, label: &str) -> OwnedValue {
    let mut properties = properties();
    put(&mut properties, "label", Value::from(label.to_owned()));
    put(&mut properties, "enabled", Value::from(false));
    structure(id, properties, Vec::new())
}

fn properties() -> zbus::zvariant::Dict<'static, 'static> {
    zbus::zvariant::Dict::new(&Signature::Str, &Signature::Variant)
}

/// Set one property. The dict's value signature is `v`, so the value has to be boxed into a
/// variant explicitly -- `Value::from` on a `Value` hands back the same value.
fn put(dict: &mut zbus::zvariant::Dict<'static, 'static>, key: &str, value: Value<'static>) {
    dict.append(Value::from(key.to_owned()), Value::Value(Box::new(value)))
        .expect("the property dict should take a string key and a variant");
}

fn structure(
    id: i32,
    properties: zbus::zvariant::Dict<'static, 'static>,
    children: Vec<OwnedValue>,
) -> OwnedValue {
    let built = StructureBuilder::new()
        .append_field(Value::from(id))
        .append_field(Value::from(properties))
        .append_field(Value::from(children))
        .build()
        .expect("the row should build");
    OwnedValue::try_from(Value::from(built)).expect("the row should be ownable")
}

/// A generated icon: a filled square with a lighter border, at 22 pixels.
///
/// ARGB32 in **network byte order** and **not premultiplied**, which is what the wire format is --
/// so this exercises `crate::pixmap`'s conversion rather than sidestepping it.
fn icon(color: (u8, u8, u8)) -> Pixmaps {
    const SIZE: i32 = 22;
    let mut bytes = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let edge = x < 2 || y < 2 || x >= SIZE - 2 || y >= SIZE - 2;
            let corner = (x < 3 && y < 3) || (x >= SIZE - 3 && y >= SIZE - 3);
            let (r, g, b) = color;
            // Transparent corners, so a mistake in the alpha handling shows up as a square block
            // rather than as something subtly wrong.
            let alpha = if corner { 0 } else { 0xff };
            let lighten = |channel: u8| channel.saturating_add(0x50);
            bytes.extend_from_slice(&if edge {
                [alpha, lighten(r), lighten(g), lighten(b)]
            } else {
                [alpha, r, g, b]
            });
        }
    }
    vec![(SIZE, SIZE, bytes)]
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut count = 2usize;
    let mut attention = false;
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--count" => {
                count = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(2)
                    .clamp(1, 16);
            }
            "--attention" => attention = true,
            "--help" | "-h" => {
                println!(
                    "fake_item [--count N] [--attention]\n\n\
                     Registers N synthetic StatusNotifierItems with the running tray.\n\
                     --attention makes the last one ask to be noticed."
                );
                return Ok(());
            }
            other => {
                eprintln!("fake_item: unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }

    let mut builder =
        zbus::blocking::connection::Builder::session()?.serve_at("/MenuBar", FakeMenu)?;
    // Item paths, in the order they will be registered. The first is the KDE-style one.
    let mut paths = Vec::new();
    for index in 0..count {
        let path = if index == 0 {
            "/StatusNotifierItem".to_owned()
        } else {
            format!("/org/ayatana/NotificationItem/fake{index}")
        };
        let last = index + 1 == count;
        builder = builder.serve_at(
            path.clone(),
            FakeItem {
                id: format!("fake{index}"),
                title: format!("Fake Item {index}"),
                status: if attention && last {
                    "NeedsAttention".to_owned()
                } else {
                    "Active".to_owned()
                },
                pixmap: icon(COLORS[index % COLORS.len()]),
            },
        )?;
        paths.push(path);
    }
    let connection = builder.build()?;

    // A bus name of its own, so the KDE-style registration has something to name. The tray talks
    // to the sender's unique name either way; this is here because a real application has one.
    let name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
    connection.request_name_with_flags(
        name.as_str(),
        zbus::fdo::RequestNameFlags::DoNotQueue | zbus::fdo::RequestNameFlags::AllowReplacement,
    )?;

    for (index, path) in paths.iter().enumerate() {
        // The first registers by *name*, the rest by *path*. See the module comment.
        let argument = if index == 0 {
            name.clone()
        } else {
            path.clone()
        };
        connection.call_method(
            Some("org.kde.StatusNotifierWatcher"),
            "/StatusNotifierWatcher",
            Some("org.kde.StatusNotifierWatcher"),
            "RegisterStatusNotifierItem",
            &(argument.as_str(),),
        )?;
        println!("registered {argument}");
    }

    println!("serving {count} item(s); Ctrl-C to remove them, kill -9 to test the crash path");
    loop {
        std::thread::park();
    }
}
