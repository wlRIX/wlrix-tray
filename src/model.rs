// SPDX-License-Identifier: GPL-3.0-or-later
//! Which items the tray knows about, which of them are shown, and in what order.
//!
//! The whole of the tray's state lives here, because the main loop owns it and nothing else does
//! -- the bus threads hold a channel and no state at all. Everything in this module is plain data
//! and arithmetic, so the ordering and filtering rules can be tested without a bus or a screen.
//!
//! # Registered is not the same as shown
//!
//! An item is registered the moment an application calls the watcher, but its properties have not
//! been read yet -- that is a round trip away, on the worker thread. Drawing a cell for an item
//! with no icon and no name would put a blank square on the desktop for as long as the read takes,
//! and a flicker of empty boxes at login is exactly the sort of thing that looks broken. So an
//! item becomes *visible* when its first read lands, and `Passive` status and a `hidden = true`
//! override each take it back out again.
//!
//! # Order has to be stable
//!
//! Items are drawn in a strip, and a strip whose contents move around is one you cannot build
//! muscle memory for. Registration order is the default because it is roughly the order the
//! programs started, which is stable across a session; `[[item]]` overrides put a chosen few in
//! front of it.

use crate::config::Config;
use crate::sni::{ItemAddress, ItemProperties, Status};

/// One hosted item.
pub struct Item {
    pub address: ItemAddress,
    pub properties: ItemProperties,
    /// Whether a read has ever landed. See the module comment.
    pub read: bool,
    /// When it registered, relative to the others. Never reused, so an item that leaves and comes
    /// back goes to the end rather than reclaiming its old place -- which is what a *restarted*
    /// application should do.
    sequence: u64,
}

impl Item {
    /// The name `[[item]]` overrides key on.
    pub fn id(&self) -> &str {
        &self.properties.id
    }
}

/// Everything the tray is hosting.
#[derive(Default)]
pub struct Items {
    all: Vec<Item>,
    next_sequence: u64,
}

impl Items {
    /// Note that an item exists. `false` when it was already known.
    pub fn register(&mut self, address: ItemAddress) -> bool {
        if self.find(&address).is_some() {
            return false;
        }
        self.next_sequence += 1;
        self.all.push(Item {
            address,
            properties: ItemProperties::default(),
            read: false,
            sequence: self.next_sequence,
        });
        true
    }

    /// Take a fresh read. `false` for an item that has since gone, whose read arrived late.
    pub fn update(&mut self, address: &ItemAddress, properties: ItemProperties) -> bool {
        let Some(index) = self.find(address) else {
            return false;
        };
        self.all[index].properties = properties;
        self.all[index].read = true;
        true
    }

    /// Drop an item. `false` when it was not there, which happens when both `NameOwnerChanged`
    /// and an explicit removal arrive for the same item.
    pub fn remove(&mut self, address: &ItemAddress) -> bool {
        let Some(index) = self.find(address) else {
            return false;
        };
        self.all.remove(index);
        true
    }

    pub fn get(&self, address: &ItemAddress) -> Option<&Item> {
        self.find(address).map(|index| &self.all[index])
    }

    pub fn is_empty(&self) -> bool {
        self.all.is_empty()
    }

    fn find(&self, address: &ItemAddress) -> Option<usize> {
        self.all.iter().position(|item| &item.address == address)
    }

    /// The items to draw, in the order to draw them.
    ///
    /// Returned as references rather than a copy of the state: the caller wants the properties to
    /// draw from, and cloning every item's pixmaps once a frame would be the most expensive thing
    /// the tray does.
    pub fn visible(&self, config: &Config) -> Vec<&Item> {
        let mut shown: Vec<&Item> = self
            .all
            .iter()
            .filter(|item| self.is_shown(item, config))
            .collect();
        // `sort_by_key` is stable, so items with no `order` keep the sequence order they are
        // already in rather than being shuffled by the sort.
        shown.sort_by_key(|item| {
            let order = config.item(item.id()).and_then(|override_| override_.order);
            // Ordered items first, and among them by their number; everything else after, by when
            // it registered. The tuple is what keeps those two groups apart without a second sort.
            (order.is_none(), order.unwrap_or(0), item.sequence)
        });
        shown
    }

    /// Whether one item makes it onto the strip.
    fn is_shown(&self, item: &Item, config: &Config) -> bool {
        if !item.read {
            return false;
        }
        if config
            .item(item.id())
            .is_some_and(|override_| override_.hidden)
        {
            return false;
        }
        item.properties.status != Status::Passive || config.show_passive
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(name: &str) -> ItemAddress {
        ItemAddress::resolve(&format!("/{name}"), &format!(":1.{}", name.len()))
    }

    fn properties(id: &str, status: Status) -> ItemProperties {
        ItemProperties {
            id: id.to_owned(),
            status,
            ..Default::default()
        }
    }

    /// Items registered and read, in the order named.
    fn hosting(ids: &[&str]) -> (Items, Vec<ItemAddress>) {
        let mut items = Items::default();
        let mut addresses = Vec::new();
        for id in ids {
            let address = address(id);
            items.register(address.clone());
            items.update(&address, properties(id, Status::Active));
            addresses.push(address);
        }
        (items, addresses)
    }

    fn config(text: &str) -> Config {
        toml::from_str(text).expect("the test config should parse")
    }

    fn shown(items: &Items, config: &Config) -> Vec<String> {
        items
            .visible(config)
            .iter()
            .map(|item| item.id().to_owned())
            .collect()
    }

    #[test]
    fn an_item_appears_only_once_its_properties_have_been_read() {
        // Otherwise a blank cell sits on the desktop for the length of a D-Bus round trip, and
        // logging in shows a row of empty boxes that fill in one by one.
        let mut items = Items::default();
        let address = address("fcitx");
        assert!(items.register(address.clone()));
        assert!(shown(&items, &config("")).is_empty());
        assert!(items.update(&address, properties("fcitx", Status::Active)));
        assert_eq!(shown(&items, &config("")), vec!["fcitx"]);
    }

    #[test]
    fn registering_twice_does_not_make_two_cells() {
        let mut items = Items::default();
        let address = address("fcitx");
        assert!(items.register(address.clone()));
        assert!(!items.register(address));
    }

    #[test]
    fn a_read_that_arrives_after_the_item_left_is_dropped() {
        // The worker thread is asynchronous by design, so this race is normal rather than
        // exceptional -- and resurrecting a dead item would leave a cell nothing can remove.
        let mut items = Items::default();
        let address = address("steam");
        items.register(address.clone());
        assert!(items.remove(&address));
        assert!(!items.update(&address, properties("steam", Status::Active)));
        assert!(items.is_empty());
    }

    #[test]
    fn passive_items_are_hidden_unless_asked_for() {
        let (mut items, addresses) = hosting(&["fcitx", "quiet"]);
        items.update(&addresses[1], properties("quiet", Status::Passive));
        assert_eq!(shown(&items, &config("")), vec!["fcitx"]);
        assert_eq!(
            shown(&items, &config("show_passive = true\n")),
            vec!["fcitx", "quiet"]
        );
    }

    #[test]
    fn an_item_asking_for_attention_is_never_hidden() {
        let (mut items, addresses) = hosting(&["loud"]);
        items.update(&addresses[0], properties("loud", Status::NeedsAttention));
        assert_eq!(shown(&items, &config("")), vec!["loud"]);
    }

    #[test]
    fn a_hidden_override_wins_over_every_status() {
        let (mut items, addresses) = hosting(&["steam"]);
        items.update(&addresses[0], properties("steam", Status::NeedsAttention));
        let config = config("[[item]]\nid = \"steam\"\nhidden = true\n");
        assert!(shown(&items, &config).is_empty());
    }

    #[test]
    fn unordered_items_keep_the_order_they_registered_in() {
        let (items, _) = hosting(&["first", "second", "third"]);
        assert_eq!(shown(&items, &config("")), vec!["first", "second", "third"]);
    }

    #[test]
    fn ordered_items_come_first_and_the_rest_keep_their_order() {
        let (items, _) = hosting(&["first", "second", "third"]);
        let config = config(
            "[[item]]\nid = \"third\"\norder = 0\n\n\
             [[item]]\nid = \"first\"\norder = 1\n",
        );
        assert_eq!(shown(&items, &config), vec!["third", "first", "second"]);
    }

    #[test]
    fn a_restarted_item_goes_to_the_end_rather_than_reclaiming_its_place() {
        // Its sequence is not reused, so it sorts after everything registered in the meantime --
        // which is what a program that just restarted should do, and it keeps the strip from
        // reshuffling under the pointer.
        let (mut items, addresses) = hosting(&["first", "second"]);
        items.remove(&addresses[0]);
        let back = address("first");
        items.register(back.clone());
        items.update(&back, properties("first", Status::Active));
        assert_eq!(shown(&items, &config("")), vec!["second", "first"]);
    }
}
