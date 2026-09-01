// SPDX-License-Identifier: GPL-3.0-or-later
//! Everything one item publishes, read off the bus in one go.
//!
//! # Why one `GetAll` and not a proxy full of typed getters
//!
//! Items are sloppy in ways a generated proxy is not forgiving about. They omit properties the
//! specification calls mandatory, publish `Menu` as a string where the specification says object
//! path, and return `InvalidArgs` for a property they have never heard of. A typed getter turns
//! any of that into an error for the whole read, and the icon simply never appears.
//!
//! So the read is one `org.freedesktop.DBus.Properties.GetAll`, and every field is pulled out of
//! the resulting map with a converter that answers a default rather than failing. What comes back
//! is always a usable [`ItemProperties`] -- possibly a threadbare one, which is the right outcome
//! for an item that published almost nothing.
//!
//! # And why there is still a fallback
//!
//! A handful of implementations answer `GetAll` with an error outright, usually because one
//! property getter of theirs throws and takes the whole call with it. Those still answer
//! individual `Get` calls, so a failed `GetAll` is retried one property at a time before the item
//! is given up on.

use std::collections::HashMap;

use zbus::zvariant::{OwnedValue, Value};

use super::{ITEM_INTERFACE, ItemAddress};

/// `org.freedesktop.DBus.Properties`, which is how everything here is read.
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// How much of the user's attention an item is asking for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Status {
    /// "Not of pressing importance to the user"; the tray may hide it, and by default does.
    Passive,
    #[default]
    Active,
    /// Asking to be noticed: drawn with the attention icon and a highlighted cell.
    NeedsAttention,
}

impl Status {
    /// Read the property's string form.
    ///
    /// Anything unrecognized is [`Status::Active`], deliberately. The alternatives are worse: a
    /// misspelling read as `Passive` hides the item with the default settings, and the user is
    /// left with an application insisting it has a tray icon and a tray insisting it does not.
    fn parse(text: &str) -> Self {
        match text {
            "Passive" => Status::Passive,
            "NeedsAttention" => Status::NeedsAttention,
            _ => Status::Active,
        }
    }
}

/// One `(width, height, ARGB32 big-endian bytes)` offer, as it comes off the bus.
pub type RawPixmap = (i32, i32, Vec<u8>);

/// What an item says when hovered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolTip {
    pub icon_name: String,
    pub title: String,
    pub description: String,
}

impl ToolTip {
    /// Whether there is anything worth putting on screen.
    pub fn is_empty(&self) -> bool {
        self.title.is_empty() && self.description.is_empty()
    }
}

/// A snapshot of one item.
#[derive(Debug, Clone, Default)]
pub struct ItemProperties {
    /// The application's own name for itself: `fcitx`, `steam`. Stable across restarts, which is
    /// what makes it the key `[[item]]` overrides use.
    pub id: String,
    pub title: String,
    pub status: Status,
    pub category: String,
    pub icon_name: String,
    pub icon_pixmap: Vec<RawPixmap>,
    pub attention_icon_name: String,
    pub attention_icon_pixmap: Vec<RawPixmap>,
    pub overlay_icon_name: String,
    pub overlay_icon_pixmap: Vec<RawPixmap>,
    /// A private directory to look `icon_name` up in first. See [`crate::icons`].
    pub icon_theme_path: String,
    pub tooltip: ToolTip,
    /// The item saying it has no activate action, only a menu -- so a *left* click should open
    /// the menu rather than call `Activate`.
    pub item_is_menu: bool,
    /// Where its `com.canonical.dbusmenu` object is, when it has one.
    pub menu: Option<String>,
}

impl ItemProperties {
    /// The icon name to draw, given the item's status.
    pub fn current_icon_name(&self) -> &str {
        if self.status == Status::NeedsAttention && !self.attention_icon_name.is_empty() {
            return &self.attention_icon_name;
        }
        &self.icon_name
    }

    /// The pixmaps to draw, given the item's status.
    pub fn current_pixmaps(&self) -> &[RawPixmap] {
        if self.status == Status::NeedsAttention && !self.attention_icon_pixmap.is_empty() {
            return &self.attention_icon_pixmap;
        }
        &self.icon_pixmap
    }

    /// What to call this item in the log and in a tooltip with nothing better.
    pub fn label(&self) -> &str {
        if !self.title.is_empty() {
            return &self.title;
        }
        &self.id
    }

    /// Build a snapshot from a property map, filling in whatever is missing.
    ///
    /// `fallback_id` is used when the item published no `Id` -- its bus name, which at least tells
    /// two anonymous items apart and gives `[[item]]` something to key on.
    pub fn from_map(map: &HashMap<String, OwnedValue>, fallback_id: &str) -> Self {
        let mut id = string(map, "Id");
        if id.is_empty() {
            id = fallback_id.to_owned();
        }
        Self {
            id,
            title: string(map, "Title"),
            status: Status::parse(&string(map, "Status")),
            category: string(map, "Category"),
            icon_name: string(map, "IconName"),
            icon_pixmap: pixmaps(map, "IconPixmap"),
            attention_icon_name: string(map, "AttentionIconName"),
            attention_icon_pixmap: pixmaps(map, "AttentionIconPixmap"),
            overlay_icon_name: string(map, "OverlayIconName"),
            overlay_icon_pixmap: pixmaps(map, "OverlayIconPixmap"),
            icon_theme_path: string(map, "IconThemePath"),
            tooltip: tooltip(map),
            item_is_menu: boolean(map, "ItemIsMenu"),
            // An item with no menu is ordinary; an item whose menu path is the root is an item
            // saying "no menu" in the roundabout way some toolkits do.
            menu: Some(string(map, "Menu")).filter(|path| path.starts_with('/') && path != "/"),
        }
    }

    /// Read every property of one item.
    pub fn read(
        connection: &zbus::blocking::Connection,
        address: &ItemAddress,
    ) -> Result<Self, String> {
        let map = match get_all(connection, address) {
            Ok(map) => map,
            // Some items' `GetAll` throws because one getter of theirs does. They still answer
            // individual `Get` calls, so the read is retried the long way before giving up.
            Err(err) => {
                let map = get_one_at_a_time(connection, address);
                if map.is_empty() {
                    return Err(err);
                }
                map
            }
        };
        Ok(Self::from_map(&map, &address.service))
    }
}

/// Every property in one call.
fn get_all(
    connection: &zbus::blocking::Connection,
    address: &ItemAddress,
) -> Result<HashMap<String, OwnedValue>, String> {
    connection
        .call_method(
            Some(address.service.as_str()),
            address.path.as_str(),
            Some(PROPERTIES),
            "GetAll",
            &(ITEM_INTERFACE,),
        )
        .map_err(|err| format!("GetAll on {}{}: {err}", address.service, address.path))?
        .body()
        .deserialize::<HashMap<String, OwnedValue>>()
        .map_err(|err| {
            format!(
                "GetAll on {}{} answered nonsense: {err}",
                address.service, address.path
            )
        })
}

/// The properties that matter, fetched one at a time.
///
/// Deliberately not every property: this path exists for items that are already misbehaving, and
/// a round trip each for eighteen properties -- most of which such an item does not have -- is a
/// lot of bus traffic to discover that. These are the ones without which there is nothing to draw.
fn get_one_at_a_time(
    connection: &zbus::blocking::Connection,
    address: &ItemAddress,
) -> HashMap<String, OwnedValue> {
    const WANTED: [&str; 9] = [
        "Id",
        "Title",
        "Status",
        "IconName",
        "IconPixmap",
        "IconThemePath",
        "AttentionIconName",
        "ItemIsMenu",
        "Menu",
    ];
    let mut map = HashMap::new();
    for name in WANTED {
        let reply = connection.call_method(
            Some(address.service.as_str()),
            address.path.as_str(),
            Some(PROPERTIES),
            "Get",
            &(ITEM_INTERFACE, name),
        );
        // A property an item does not have is an error here, and a perfectly ordinary one.
        if let Ok(reply) = reply
            && let Ok(value) = reply.body().deserialize::<OwnedValue>()
        {
            map.insert(name.to_owned(), value);
        }
    }
    map
}

/// A string property, however it was typed.
///
/// `Menu` is the reason this is not a plain downcast: the specification says object path, and
/// implementations send both that and a plain string. Reading only one of the two loses every
/// menu published by the other half of the ecosystem.
fn string(map: &HashMap<String, OwnedValue>, key: &str) -> String {
    match map.get(key).map(|value| &**value) {
        Some(Value::Str(text)) => text.to_string(),
        Some(Value::ObjectPath(path)) => path.to_string(),
        Some(Value::Signature(signature)) => signature.to_string(),
        _ => String::new(),
    }
}

/// A boolean property, defaulting to false.
fn boolean(map: &HashMap<String, OwnedValue>, key: &str) -> bool {
    matches!(map.get(key).map(|value| &**value), Some(Value::Bool(true)))
}

/// An `a(iiay)` property.
fn pixmaps(map: &HashMap<String, OwnedValue>, key: &str) -> Vec<RawPixmap> {
    let Some(Value::Array(array)) = map.get(key).map(|value| &**value) else {
        return Vec::new();
    };
    array.iter().filter_map(one_pixmap).collect()
}

/// One `(ii ay)` structure.
fn one_pixmap(value: &Value<'_>) -> Option<RawPixmap> {
    let Value::Structure(fields) = value else {
        return None;
    };
    let fields = fields.fields();
    let (Some(Value::I32(width)), Some(Value::I32(height)), Some(Value::Array(bytes))) =
        (fields.first(), fields.get(1), fields.get(2))
    else {
        return None;
    };
    let bytes = bytes
        .iter()
        .map(|byte| match byte {
            Value::U8(byte) => *byte,
            _ => 0,
        })
        .collect();
    Some((*width, *height, bytes))
}

/// The `(sa(iiay)ss)` tooltip property.
///
/// The pixmap half is read past rather than kept: a tooltip's own icon is a nicety no wlRIX
/// tooltip draws, and holding a second copy of every item's artwork for it would not be free.
fn tooltip(map: &HashMap<String, OwnedValue>) -> ToolTip {
    let Some(Value::Structure(fields)) = map.get("ToolTip").map(|value| &**value) else {
        return ToolTip::default();
    };
    let fields = fields.fields();
    let text = |index: usize| match fields.get(index) {
        Some(Value::Str(text)) => text.to_string(),
        _ => String::new(),
    };
    ToolTip {
        icon_name: text(0),
        title: text(2),
        description: text(3),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: Vec<(&str, Value<'static>)>) -> HashMap<String, OwnedValue> {
        pairs
            .into_iter()
            .map(|(key, value)| (key.to_owned(), OwnedValue::try_from(value).unwrap()))
            .collect()
    }

    #[test]
    fn an_unknown_status_shows_the_item_rather_than_hiding_it() {
        assert_eq!(Status::parse("Passive"), Status::Passive);
        assert_eq!(Status::parse("NeedsAttention"), Status::NeedsAttention);
        assert_eq!(Status::parse("Active"), Status::Active);
        // A misspelling must not hide the icon: the user would be left with an application
        // insisting it has a tray icon and a tray insisting it does not.
        assert_eq!(Status::parse("active"), Status::Active);
        assert_eq!(Status::parse(""), Status::Active);
    }

    #[test]
    fn a_menu_path_is_read_whether_it_was_typed_as_a_path_or_a_string() {
        // fcitx5 sends an object path; several toolkits send a plain string. Reading only one of
        // the two loses every menu published by the other half of the ecosystem.
        for value in [
            Value::from(zbus::zvariant::ObjectPath::try_from("/MenuBar").unwrap()),
            Value::from("/MenuBar"),
        ] {
            let properties = ItemProperties::from_map(&map(vec![("Menu", value)]), ":1.1");
            assert_eq!(properties.menu.as_deref(), Some("/MenuBar"));
        }
    }

    #[test]
    fn no_menu_and_a_root_menu_both_mean_no_menu() {
        assert_eq!(ItemProperties::from_map(&map(vec![]), ":1.1").menu, None);
        let root = map(vec![("Menu", Value::from("/"))]);
        assert_eq!(ItemProperties::from_map(&root, ":1.1").menu, None);
    }

    #[test]
    fn a_missing_id_falls_back_to_something_that_tells_items_apart() {
        // `[[item]]` overrides key on the id, and an empty key would apply to every anonymous
        // item at once.
        let properties = ItemProperties::from_map(&map(vec![]), ":1.42");
        assert_eq!(properties.id, ":1.42");
    }

    #[test]
    fn an_item_that_published_almost_nothing_still_reads() {
        // The failure this guards against is a whole read thrown away because one mandatory
        // property was absent -- which is most items, in practice.
        let properties =
            ItemProperties::from_map(&map(vec![("IconName", Value::from("fcitx"))]), ":1.1");
        assert_eq!(properties.icon_name, "fcitx");
        assert_eq!(properties.status, Status::Active);
        assert!(properties.title.is_empty());
        assert!(!properties.item_is_menu);
        assert!(properties.tooltip.is_empty());
    }

    #[test]
    fn the_attention_icon_wins_only_while_attention_is_wanted() {
        let mut properties = ItemProperties {
            icon_name: "quiet".into(),
            attention_icon_name: "loud".into(),
            ..Default::default()
        };
        assert_eq!(properties.current_icon_name(), "quiet");
        properties.status = Status::NeedsAttention;
        assert_eq!(properties.current_icon_name(), "loud");
        // ...and an item that asks for attention without publishing an attention icon keeps the
        // one it has, rather than going blank.
        properties.attention_icon_name.clear();
        assert_eq!(properties.current_icon_name(), "quiet");
    }

    #[test]
    fn a_tooltip_reads_its_title_and_description_past_the_pixmap_field() {
        // `(s a(iiay) s s)` -- the two strings that matter are the third and fourth fields, and
        // reading them as the first and second is the classic way to show an icon name to a user.
        let value = Value::from(zbus::zvariant::Structure::from((
            "icon-name",
            Vec::<(i32, i32, Vec<u8>)>::new(),
            "Mozc",
            "Japanese input",
        )));
        let properties = ItemProperties::from_map(&map(vec![("ToolTip", value)]), ":1.1");
        assert_eq!(properties.tooltip.title, "Mozc");
        assert_eq!(properties.tooltip.description, "Japanese input");
        assert!(!properties.tooltip.is_empty());
    }

    #[test]
    fn pixmaps_come_back_with_their_bytes_intact() {
        let offers: Vec<(i32, i32, Vec<u8>)> = vec![(1, 1, vec![0xff, 0x10, 0x20, 0x30])];
        let properties =
            ItemProperties::from_map(&map(vec![("IconPixmap", Value::from(offers))]), ":1.1");
        assert_eq!(properties.icon_pixmap.len(), 1);
        assert_eq!(properties.icon_pixmap[0].0, 1);
        assert_eq!(properties.icon_pixmap[0].2, vec![0xff, 0x10, 0x20, 0x30]);
    }
}
