// SPDX-License-Identifier: GPL-3.0-or-later
//! `org.kde.StatusNotifierWatcher`: the interface applications look for before they publish.
//!
//! The whole of it is a register-and-announce: an application calls
//! `RegisterStatusNotifierItem`, the watcher remembers where the item is and says so, and
//! everything interesting happens afterwards over on the item's own interface.
//!
//! The one piece of judgment here is that the tray answers `IsStatusNotifierHostRegistered` with
//! a flat `true`. That property means "somebody is displaying items"; this program is that
//! somebody, from the moment the object is served, and it cannot stop being it while it is
//! running. Several toolkits publish nothing at all until the property is true, so a watcher that
//! answered `false` until some separate host turned up would be a tray that stayed empty.

use std::sync::Arc;

use zbus::object_server::SignalEmitter;

use super::{ItemAddress, Registry, TrayEvent};

/// The version of the specification this implements. `0` is what every implementation reports;
/// the field has never been used to negotiate anything.
const PROTOCOL_VERSION: i32 = 0;

pub struct Watcher {
    pub registry: Arc<Registry>,
    pub sender: calloop::channel::Sender<TrayEvent>,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    /// An application is offering an item.
    ///
    /// `service` is either a bus name or an object path -- see [`ItemAddress::resolve`], which is
    /// where that fork is resolved and where the reason it matters is written down.
    async fn register_status_notifier_item(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        service: String,
    ) {
        let Some(sender) = header.sender() else {
            // Only possible off a bus, where there is nobody to talk back to anyway.
            return;
        };
        let address = ItemAddress::resolve(&service, sender.as_str());
        if !self.registry.add(address.clone()) {
            // Already known. An application re-registers when it sees the watcher name reappear,
            // and honoring that twice would put two cells on screen for one program.
            return;
        }
        let listed = format!("{}{}", address.service, address.path);
        let _ = self.sender.send(TrayEvent::Registered(address));
        let _ = Self::status_notifier_item_registered(&emitter, listed).await;
    }

    /// Another host is displaying items too. Nothing here depends on it; the announcement is what
    /// the applications are listening for, so it is passed on.
    async fn register_status_notifier_host(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        _service: String,
    ) {
        let _ = Self::status_notifier_host_registered(&emitter).await;
    }

    /// Every item, as `<bus name><object path>`.
    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.registry.listed()
    }

    /// Always true. See the module comment.
    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        PROTOCOL_VERSION
    }

    #[zbus(signal)]
    async fn status_notifier_item_registered(
        emitter: &SignalEmitter<'_>,
        service: String,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_item_unregistered(
        emitter: &SignalEmitter<'_>,
        service: String,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_host_registered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// Say that a host exists, now that one does.
///
/// Emitted from outside the interface because it happens at startup, before anything has called
/// in -- and through the connection rather than a [`SignalEmitter`] because the blocking API has
/// no way to await one from a synchronous caller.
pub fn announce_host(connection: &zbus::blocking::Connection, host: &str) {
    if let Err(err) = connection.emit_signal(
        None::<&str>,
        super::WATCHER_PATH,
        super::WATCHER_NAME,
        "StatusNotifierHostRegistered",
        &(),
    ) {
        eprintln!("wlrix-tray: could not announce {host}: {err}");
    }
}

/// Say that an item is gone, so any other host stops drawing it.
pub fn announce_item_gone(connection: &zbus::blocking::Connection, listed: &str) {
    let _ = connection.emit_signal(
        None::<&str>,
        super::WATCHER_PATH,
        super::WATCHER_NAME,
        "StatusNotifierItemUnregistered",
        &(listed,),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::object_server::Interface;

    /// The introspection XML the interface actually serves.
    fn introspection() -> String {
        let (sender, _channel) = calloop::channel::channel();
        let watcher = Watcher {
            registry: Arc::new(Registry::default()),
            sender,
        };
        let mut xml = String::new();
        watcher.introspect_to_writer(&mut xml, 0);
        xml
    }

    #[test]
    fn the_interface_is_the_one_applications_look_for() {
        assert_eq!(Watcher::name(), super::super::WATCHER_NAME);
    }

    #[test]
    fn every_member_an_item_needs_is_served() {
        // fcitx5 and libappindicator between them touch all of these. A rename in the Rust
        // method names would silently change the D-Bus names, and the failure mode is an empty
        // tray with nothing in the log -- an application that gets `UnknownMethod` from the
        // watcher gives up quietly.
        let xml = introspection();
        for member in [
            "RegisterStatusNotifierItem",
            "RegisterStatusNotifierHost",
            "RegisteredStatusNotifierItems",
            "IsStatusNotifierHostRegistered",
            "ProtocolVersion",
            "StatusNotifierItemRegistered",
            "StatusNotifierItemUnregistered",
            "StatusNotifierHostRegistered",
        ] {
            assert!(
                xml.contains(&format!("\"{member}\"")),
                "{member} is missing:\n{xml}"
            );
        }
    }
}
