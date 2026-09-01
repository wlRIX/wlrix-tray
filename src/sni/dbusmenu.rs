// SPDX-License-Identifier: GPL-3.0-or-later
//! `com.canonical.dbusmenu`: the menu behind an item's right click.
//!
//! This is the half of the tray that makes it useful rather than decorative. fcitx5's menu is the
//! list of configured input methods -- it is how you switch between Mozc and plain Latin without
//! a keybind -- and it exists only here. An item's own `ContextMenu` method is what a tray calls
//! when it *cannot* speak this protocol, and on Wayland almost every application answers it by
//! doing nothing at all.
//!
//! # `AboutToShow` is not optional
//!
//! `AboutToShow(id)` is the call that tells an application its menu is about to be looked at, and
//! several applications populate the menu *in* it. fcitx5 is one: skip the call and `GetLayout`
//! answers with a correctly-formed, entirely empty menu. It is called for the root before the
//! layout is fetched, and again for any submenu that turns out to be empty.
//!
//! # The layout, and the shapes it arrives in
//!
//! `GetLayout(parent, depth, properties)` answers `(u(ia{sv}av))`: a revision, then a recursive
//! `(id, properties, children)` structure whose children are *variants* wrapping more of the
//! same. Depth `-1` asks for the whole tree in one round trip, and an empty property list asks
//! for every property, which together is one call for an entire menu.
//!
//! [`parse`] is pure, so the shapes real applications send can be tested without a bus.

use std::collections::HashMap;

use zbus::zvariant::{OwnedValue, Value};

use super::{ItemAddress, MENU_INTERFACE};

/// How deep a chain of empty submenus is followed before giving up.
///
/// Filling an empty submenu means another `AboutToShow` and another `GetLayout`, and each of those
/// may reveal another empty submenu. A menu that nested that far would be unusable anyway; the cap
/// is here so a buggy or hostile item cannot keep the worker thread busy forever.
const MAX_FILL_DEPTH: u32 = 4;

/// What a row does when it is chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Toggle {
    #[default]
    None,
    /// A checkbox. `None` is the specification's "indeterminate".
    Check(Option<bool>),
    /// One of a set. Same three states, for the same reason.
    Radio(Option<bool>),
}

/// One row of a menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The id to pass back to `Event`. Meaningless to anyone but the application.
    pub id: i32,
    pub label: String,
    pub enabled: bool,
    pub separator: bool,
    pub toggle: Toggle,
    /// An icon name for the row, when the application gave one.
    pub icon_name: String,
    pub children: Vec<Entry>,
    /// Whether the application says this row opens a submenu, which it may say before it has
    /// filled one in.
    pub submenu: bool,
}

impl Entry {
    /// Whether this row can be chosen. A separator cannot, a disabled row cannot, and a submenu
    /// row opens rather than acts.
    pub fn selectable(&self) -> bool {
        self.enabled && !self.separator && !self.submenu
    }
}

/// A whole menu, ready to lay out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    /// Where it came from, so a chosen row can be sent back to the right object.
    pub path: String,
    pub entries: Vec<Entry>,
}

impl Menu {
    /// Whether there is anything to post. An item with an empty menu gets none.
    pub fn is_empty(&self) -> bool {
        self.entries.iter().all(|entry| entry.separator)
    }
}

/// Ask an item for its menu: `AboutToShow`, then `GetLayout`, then fill any empty submenus.
pub fn fetch(
    connection: &zbus::blocking::Connection,
    address: &ItemAddress,
    path: &str,
) -> Result<Menu, String> {
    // The return value says "the layout changed", which is what happens next regardless -- and an
    // item that has never heard of the call answers with an error, which is not a reason to give
    // up on its menu.
    let _ = about_to_show(connection, address, path, 0);

    let mut entries = layout(connection, address, path, 0)?;
    fill_submenus(connection, address, path, &mut entries, 0);
    Ok(Menu {
        path: path.to_owned(),
        entries,
    })
}

/// Tell a menu it is about to be shown, so it can populate itself.
fn about_to_show(
    connection: &zbus::blocking::Connection,
    address: &ItemAddress,
    path: &str,
    id: i32,
) -> Result<bool, String> {
    connection
        .call_method(
            Some(address.service.as_str()),
            path,
            Some(MENU_INTERFACE),
            "AboutToShow",
            &(id,),
        )
        .map_err(|err| format!("AboutToShow({id}): {err}"))?
        .body()
        .deserialize::<bool>()
        .map_err(|err| format!("AboutToShow({id}) answered nonsense: {err}"))
}

/// One `GetLayout` from `id` downward.
fn layout(
    connection: &zbus::blocking::Connection,
    address: &ItemAddress,
    path: &str,
    id: i32,
) -> Result<Vec<Entry>, String> {
    // `-1` is the whole subtree, and an empty property list is every property. One round trip for
    // a menu, rather than one per row.
    let reply = connection
        .call_method(
            Some(address.service.as_str()),
            path,
            Some(MENU_INTERFACE),
            "GetLayout",
            &(id, -1i32, Vec::<String>::new()),
        )
        .map_err(|err| format!("GetLayout({id}) on {path}: {err}"))?;
    let (_revision, root) = reply
        .body()
        .deserialize::<(u32, (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>))>()
        .map_err(|err| format!("GetLayout({id}) on {path} answered nonsense: {err}"))?;
    Ok(parse(&root.2))
}

/// Ask again for any submenu the item declared but did not fill in.
///
/// Applications that build a submenu in its own `AboutToShow` are common enough that a tray which
/// only ever calls it for the root shows empty submenus and looks broken.
fn fill_submenus(
    connection: &zbus::blocking::Connection,
    address: &ItemAddress,
    path: &str,
    entries: &mut [Entry],
    depth: u32,
) {
    if depth >= MAX_FILL_DEPTH {
        return;
    }
    for entry in entries.iter_mut() {
        if entry.submenu && entry.children.is_empty() {
            let _ = about_to_show(connection, address, path, entry.id);
            if let Ok(children) = layout(connection, address, path, entry.id) {
                entry.children = children;
            }
        }
        fill_submenus(connection, address, path, &mut entry.children, depth + 1);
    }
}

/// Tell a row it was chosen.
pub fn choose(
    connection: &zbus::blocking::Connection,
    address: &ItemAddress,
    path: &str,
    id: i32,
) -> Result<(), String> {
    // The timestamp is documented as "the time the event happened", in seconds since the epoch.
    // Nothing consumes it in practice, and an item that did would still rather have a plausible
    // number than a zero.
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as u32);
    connection
        .call_method(
            Some(address.service.as_str()),
            path,
            Some(MENU_INTERFACE),
            "Event",
            &(id, "clicked", Value::from(0i32), timestamp),
        )
        .map(|_| ())
        .map_err(|err| format!("Event({id}, clicked) on {path}: {err}"))
}

/// Turn a `GetLayout` child list into rows.
///
/// Invisible rows are dropped here rather than at draw time, so nothing downstream has to
/// remember that a row it can see in the tree is one the user must not.
pub fn parse(children: &[OwnedValue]) -> Vec<Entry> {
    children
        .iter()
        .filter_map(|child| entry(peel(child)))
        .collect()
}

/// A variant may or may not still be wrapped by the time it reaches here, depending on how the
/// sender encoded it. Peeling until it is not costs nothing and saves a whole menu.
fn peel<'a>(value: &'a Value<'a>) -> &'a Value<'a> {
    let mut value = value;
    while let Value::Value(inner) = value {
        value = inner;
    }
    value
}

/// One `(ia{sv}av)` structure.
fn entry(value: &Value<'_>) -> Option<Entry> {
    let Value::Structure(fields) = value else {
        return None;
    };
    let fields = fields.fields();
    let Some(Value::I32(id)) = fields.first().map(peel) else {
        return None;
    };
    let properties: HashMap<String, Value<'_>> = match fields.get(1).map(peel) {
        Some(Value::Dict(dict)) => dict
            .iter()
            .filter_map(|(key, value)| match peel(key) {
                Value::Str(key) => Some((key.to_string(), value.try_clone().ok()?)),
                _ => None,
            })
            .collect(),
        _ => HashMap::new(),
    };

    // Absent means visible, per the specification's defaults -- and most rows say nothing at all,
    // so reading a missing key as `false` would empty every menu on the bus.
    if !flag(&properties, "visible", true) {
        return None;
    }

    let children = match fields.get(2).map(peel) {
        Some(Value::Array(array)) => array
            .iter()
            .filter_map(|child| entry(peel(child)))
            .collect(),
        _ => Vec::new(),
    };

    let separator = text(&properties, "type") == "separator";
    Some(Entry {
        id: *id,
        label: mnemonic(&text(&properties, "label")),
        enabled: flag(&properties, "enabled", true),
        separator,
        toggle: toggle(&properties),
        icon_name: text(&properties, "icon-name"),
        submenu: !separator && text(&properties, "children-display") == "submenu",
        children,
    })
}

/// A string property, or empty.
fn text(properties: &HashMap<String, Value<'_>>, key: &str) -> String {
    match properties.get(key).map(peel) {
        Some(Value::Str(text)) => text.to_string(),
        _ => String::new(),
    }
}

/// A boolean property, or `absent` when it is not there.
fn flag(properties: &HashMap<String, Value<'_>>, key: &str, absent: bool) -> bool {
    match properties.get(key).map(peel) {
        Some(Value::Bool(value)) => *value,
        _ => absent,
    }
}

/// The `toggle-type` and `toggle-state` pair.
fn toggle(properties: &HashMap<String, Value<'_>>) -> Toggle {
    // `-1` is the specification's "indeterminate", and anything other than 0 or 1 means the same
    // thing: the application does not know, so neither does the tray.
    let state = match properties.get("toggle-state").map(peel) {
        Some(Value::I32(0)) => Some(false),
        Some(Value::I32(1)) => Some(true),
        _ => None,
    };
    match text(properties, "toggle-type").as_str() {
        "checkmark" => Toggle::Check(state),
        "radio" => Toggle::Radio(state),
        _ => Toggle::None,
    }
}

/// Strip the mnemonic markers out of a label.
///
/// A single `_` marks the next character as the accelerator and is not shown; a doubled `__` is a
/// literal underscore. Leaving them in gives menus full of `_Configure`, which is what a tray that
/// forgets this looks like.
fn mnemonic(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut chars = label.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '_' {
            out.push(character);
            continue;
        }
        if chars.peek() == Some(&'_') {
            chars.next();
            out.push('_');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::{Dict, Signature, StructureBuilder};

    /// A `(ia{sv}av)` row, as `GetLayout` sends one.
    fn row(
        id: i32,
        properties: Vec<(&str, Value<'static>)>,
        children: Vec<Value<'static>>,
    ) -> Value<'static> {
        let mut dict = Dict::new(&Signature::Str, &Signature::Variant);
        for (key, value) in properties {
            // Wrapped explicitly: the dict's value signature is `v`, and `Value::from` on a
            // `Value` hands back the same value rather than boxing it into a variant.
            dict.append(Value::from(key.to_owned()), Value::Value(Box::new(value)))
                .expect("the property dict should take a string key and a variant");
        }
        // Built field by field rather than from a tuple: a `Dict` is not a `Type`, so it has to
        // go in as an already-erased `Value`.
        Value::from(
            StructureBuilder::new()
                .append_field(Value::from(id))
                .append_field(Value::from(dict))
                .append_field(Value::from(children))
                .build()
                .expect("the row should build"),
        )
    }

    fn parsed(children: Vec<Value<'static>>) -> Vec<Entry> {
        let owned: Vec<OwnedValue> = children
            .into_iter()
            .map(|value| OwnedValue::try_from(value).unwrap())
            .collect();
        parse(&owned)
    }

    #[test]
    fn a_row_with_no_properties_is_visible_and_enabled() {
        // Both default to true in the specification, and most rows say nothing at all -- reading
        // a missing key as false would empty every menu on the bus.
        let entries = parsed(vec![row(1, vec![], vec![])]);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].enabled);
        assert!(!entries[0].separator);
        assert!(entries[0].selectable());
    }

    #[test]
    fn an_invisible_row_is_dropped_rather_than_drawn_gray() {
        let entries = parsed(vec![
            row(1, vec![("visible", Value::from(false))], vec![]),
            row(2, vec![], vec![]),
        ]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, 2);
    }

    #[test]
    fn mnemonics_do_not_reach_the_screen() {
        assert_eq!(mnemonic("_Configure"), "Configure");
        assert_eq!(mnemonic("Res_tart"), "Restart");
        // A doubled underscore is a literal one.
        assert_eq!(mnemonic("wl__rix"), "wl_rix");
        assert_eq!(mnemonic("plain"), "plain");
        // A trailing marker marks nothing, and must not panic.
        assert_eq!(mnemonic("trailing_"), "trailing");
    }

    #[test]
    fn separators_and_toggles_are_read() {
        let entries = parsed(vec![
            row(1, vec![("type", Value::from("separator"))], vec![]),
            row(
                2,
                vec![
                    ("label", Value::from("Mozc")),
                    ("toggle-type", Value::from("radio")),
                    ("toggle-state", Value::from(1i32)),
                ],
                vec![],
            ),
            row(
                3,
                vec![
                    ("toggle-type", Value::from("checkmark")),
                    ("toggle-state", Value::from(-1i32)),
                ],
                vec![],
            ),
        ]);
        assert!(entries[0].separator);
        assert!(!entries[0].selectable(), "a separator is not a choice");
        assert_eq!(entries[1].toggle, Toggle::Radio(Some(true)));
        assert_eq!(entries[1].label, "Mozc");
        // Indeterminate, which is a state the specification has and a checkbox has to survive.
        assert_eq!(entries[2].toggle, Toggle::Check(None));
    }

    #[test]
    fn a_disabled_row_is_kept_but_cannot_be_chosen() {
        // Kept, because a menu that silently loses its grayed-out rows is a menu whose items move
        // around depending on state.
        let entries = parsed(vec![row(1, vec![("enabled", Value::from(false))], vec![])]);
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].selectable());
    }

    #[test]
    fn a_submenu_row_carries_its_children_and_is_not_itself_a_choice() {
        let entries = parsed(vec![row(
            1,
            vec![
                ("label", Value::from("Input Method")),
                ("children-display", Value::from("submenu")),
            ],
            vec![row(2, vec![("label", Value::from("Mozc"))], vec![])],
        )]);
        assert!(entries[0].submenu);
        assert!(
            !entries[0].selectable(),
            "a submenu row opens, it does not act"
        );
        assert_eq!(entries[0].children.len(), 1);
        assert_eq!(entries[0].children[0].label, "Mozc");
    }

    #[test]
    fn a_declared_but_empty_submenu_is_still_marked_as_one() {
        // This is the shape fcitx5 sends before `AboutToShow`, and the flag is what tells the
        // worker to go back and ask again.
        let entries = parsed(vec![row(
            1,
            vec![("children-display", Value::from("submenu"))],
            vec![],
        )]);
        assert!(entries[0].submenu);
        assert!(entries[0].children.is_empty());
    }

    #[test]
    fn a_menu_of_nothing_but_separators_counts_as_empty() {
        // Posting it would put an empty beveled box on the desktop, which reads as a bug.
        let menu = Menu {
            path: "/MenuBar".into(),
            entries: parsed(vec![row(
                1,
                vec![("type", Value::from("separator"))],
                vec![],
            )]),
        };
        assert!(menu.is_empty());
    }

    /// fcitx5's actual menu, transcribed from a `GetLayout(0, -1, [])` against fcitx5 5.1.21.
    ///
    /// Worth having verbatim rather than paraphrased. It is the menu this component exists for,
    /// and two things about it are not what a reading of the specification would lead you to
    /// build for: the input methods are **top-level radio rows**, not a submenu, and the *root*
    /// carries `children-display = "submenu"` while its children do not.
    #[test]
    fn fcitx5s_real_menu_parses() {
        fn method(id: i32, label: &'static str, icon: &'static str, on: i32) -> Value<'static> {
            row(
                id,
                vec![
                    ("label", Value::from(label)),
                    ("icon-name", Value::from(icon)),
                    ("toggle-type", Value::from("radio")),
                    ("toggle-state", Value::from(on)),
                ],
                vec![],
            )
        }
        fn action(id: i32, label: &'static str) -> Value<'static> {
            row(id, vec![("label", Value::from(label))], vec![])
        }
        let entries = parsed(vec![
            method(100, "キーボード - 日本語", "input-keyboard", 1),
            method(101, "Mozc", "fcitx_mozc", 0),
            row(2, vec![("type", Value::from("separator"))], vec![]),
            action(5, "再起動"),
            action(4, "入力メソッドの設定"),
            action(6, "閉じる"),
        ]);

        assert_eq!(entries.len(), 6);
        // The two input methods, with the active one marked -- this is the whole point of the
        // tray for an input method, and the ids are what `Event` sends back to switch.
        assert_eq!(entries[0].toggle, Toggle::Radio(Some(true)));
        assert_eq!(entries[1].toggle, Toggle::Radio(Some(false)));
        assert_eq!(entries[1].id, 101);
        assert!(entries[0].selectable() && entries[1].selectable());
        // Not submenus, whatever the root said about itself.
        assert!(entries.iter().all(|entry| !entry.submenu));
        assert!(entries[2].separator);
        // The labels survive as they arrived. A tray that mangles these is a tray a Japanese
        // user cannot read.
        assert_eq!(entries[3].label, "再起動");
        assert_eq!(entries[5].label, "閉じる");
    }

    #[test]
    fn junk_in_the_child_list_is_skipped_rather_than_fatal() {
        let owned = vec![
            OwnedValue::try_from(Value::from("not a row")).unwrap(),
            OwnedValue::try_from(row(7, vec![], vec![])).unwrap(),
        ];
        let entries = parse(&owned);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, 7);
    }
}
