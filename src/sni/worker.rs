// SPDX-License-Identifier: GPL-3.0-or-later
//! The two threads that talk to items.
//!
//! # The signal thread
//!
//! One [`zbus::blocking::MessageIterator`] over the whole connection, with four match rules added
//! by hand. A proxy per item was the obvious alternative and is worse: items appear and vanish
//! faster than the main loop notices, a proxy bound to a name that has gone is an object that can
//! only report errors, and the bookkeeping to create and drop one per item is exactly the
//! bookkeeping this avoids. A match rule naming an *interface* covers every item there will ever
//! be, including the ones that have not registered yet.
//!
//! ## Every change signal means the same thing: read it all again
//!
//! `NewIcon`, `NewAttentionIcon`, `NewOverlayIcon`, `NewToolTip` and `NewTitle` carry **no
//! arguments** -- they say only that something changed. `NewStatus` carries the new status, and
//! Ayatana-derived items emit none of them, sending a plain
//! `org.freedesktop.DBus.Properties.PropertiesChanged` instead. Handling each signal specially
//! would mean five nearly identical paths and a sixth for the items that use none of them, so
//! every one of them turns into the same [`TrayEvent::Changed`] and the loop asks for a re-read.
//!
//! ## And why `NameOwnerChanged` is the one that matters
//!
//! An application that exits politely calls nothing on the way out; the specification has no
//! unregister method at all. What actually happens is that its bus name loses its owner, so that
//! is what removes an item -- and it is the only thing that does. A tray that waits for a polite
//! goodbye accumulates dead icons all session.
//!
//! # The worker thread
//!
//! Every read from and call to an item, off the main loop. See [`super`] for why this is not done
//! from the loop the way the portal does it.

use std::sync::Arc;

use zbus::MatchRule;
use zbus::blocking::MessageIterator;
use zbus::message::Type;

use super::{
    Command, ITEM_INTERFACE, ItemAddress, MENU_INTERFACE, Registry, TrayEvent, dbusmenu, item,
    watcher,
};

/// `org.freedesktop.DBus.Properties`.
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
/// The bus's own interface, where `NameOwnerChanged` comes from.
const DBUS: &str = "org.freedesktop.DBus";

/// Subscribe to everything the tray cares about, and report it into the main loop.
pub fn start(
    connection: &zbus::blocking::Connection,
    registry: Arc<Registry>,
    sender: calloop::channel::Sender<TrayEvent>,
) {
    let connection = connection.clone();
    std::thread::Builder::new()
        .name("wlrix-tray-signals".to_owned())
        .spawn(move || {
            if let Err(err) = subscribe(&connection) {
                // Not fatal, and worth being precise about in the log: items would still appear,
                // because registration is a method call rather than a signal, but they would
                // never change their icon and never go away.
                eprintln!(
                    "wlrix-tray: could not subscribe to item signals ({err}); icons will not \
                     update or disappear"
                );
                return;
            }
            for message in MessageIterator::from(&connection) {
                let Ok(message) = message else { continue };
                if !dispatch(&connection, &registry, &sender, &message) {
                    // The channel is closed, which means the main loop has gone: so should this.
                    return;
                }
            }
        })
        .expect("the signal thread should start");
}

/// Add the four match rules. Without these the bus sends nothing.
fn subscribe(connection: &zbus::blocking::Connection) -> zbus::Result<()> {
    let bus = zbus::blocking::fdo::DBusProxy::new(connection)?;
    // Every `New…` signal an item emits.
    bus.add_match_rule(
        MatchRule::builder()
            .msg_type(Type::Signal)
            .interface(ITEM_INTERFACE)?
            .build(),
    )?;
    // What the Ayatana-derived items emit instead. `arg0` narrows it to item properties, so the
    // tray is not woken by every property change on the session bus.
    bus.add_match_rule(
        MatchRule::builder()
            .msg_type(Type::Signal)
            .interface(PROPERTIES)?
            .member("PropertiesChanged")?
            .add_arg(ITEM_INTERFACE)?
            .build(),
    )?;
    // The only reliable notice that an item is gone.
    bus.add_match_rule(
        MatchRule::builder()
            .msg_type(Type::Signal)
            .sender(DBUS)?
            .interface(DBUS)?
            .member("NameOwnerChanged")?
            .build(),
    )?;
    // `LayoutUpdated` and `ItemsPropertiesUpdated`, for a menu that changes while it is open.
    bus.add_match_rule(
        MatchRule::builder()
            .msg_type(Type::Signal)
            .interface(MENU_INTERFACE)?
            .build(),
    )?;
    Ok(())
}

/// Turn one signal into events. `false` when the main loop has gone.
fn dispatch(
    connection: &zbus::blocking::Connection,
    registry: &Registry,
    sender: &calloop::channel::Sender<TrayEvent>,
    message: &zbus::Message,
) -> bool {
    let header = message.header();
    let interface = header.interface().map(|name| name.to_string());
    let member = header.member().map(|name| name.to_string());
    let from = header.sender().map(|name| name.to_string());

    match (interface.as_deref(), member.as_deref()) {
        (Some(DBUS), Some("NameOwnerChanged")) => {
            let Ok((name, _old, new)) = message.body().deserialize::<(String, String, String)>()
            else {
                return true;
            };
            // An empty new owner is the name going away. A *changed* owner would mean the same
            // item served by a different process, which unique names never do.
            if !new.is_empty() {
                return true;
            }
            for address in registry.remove_service(&name) {
                watcher::announce_item_gone(
                    connection,
                    &format!("{}{}", address.service, address.path),
                );
                if sender.send(TrayEvent::Gone(address)).is_err() {
                    return false;
                }
            }
            true
        }
        (Some(ITEM_INTERFACE), _) | (Some(PROPERTIES), Some("PropertiesChanged")) => {
            changed(registry, sender, from.as_deref(), TrayEvent::Changed)
        }
        (Some(MENU_INTERFACE), _) => {
            changed(registry, sender, from.as_deref(), TrayEvent::MenuStale)
        }
        _ => true,
    }
}

/// Report something about every item a bus name serves. `false` when the main loop has gone.
fn changed(
    registry: &Registry,
    sender: &calloop::channel::Sender<TrayEvent>,
    from: Option<&str>,
    event: fn(ItemAddress) -> TrayEvent,
) -> bool {
    let Some(from) = from else { return true };
    // The signal may be from any program on the bus -- the interface match rules are not
    // sender-specific, because the senders are not known in advance.
    for address in registry.for_service(from) {
        if sender.send(event(address)).is_err() {
            return false;
        }
    }
    true
}

/// Start the thread that reads from and calls into items.
///
/// Answers go back on `sender`, the same calloop channel the signal thread reports on -- so the
/// main loop has one source for everything the bus has to say, rather than one per thread.
pub fn start_worker(
    connection: &zbus::blocking::Connection,
    sender: calloop::channel::Sender<TrayEvent>,
) -> std::sync::mpsc::Sender<Command> {
    let (commands, requests) = std::sync::mpsc::channel::<Command>();
    let connection = connection.clone();
    std::thread::Builder::new()
        .name("wlrix-tray-items".to_owned())
        .spawn(move || {
            for command in requests {
                if matches!(command, Command::Stop) {
                    return;
                }
                run(&connection, &sender, command);
            }
        })
        .expect("the item thread should start");
    commands
}

/// Do one command.
fn run(
    connection: &zbus::blocking::Connection,
    sender: &calloop::channel::Sender<TrayEvent>,
    command: Command,
) {
    let answer = |event: TrayEvent| {
        let _ = sender.send(event);
    };
    match command {
        Command::Read(address) => match item::ItemProperties::read(connection, &address) {
            Ok(properties) => answer(TrayEvent::Properties(address, Box::new(properties))),
            Err(err) => {
                // Not a removal. An item that fails one read is usually mid-startup and answers
                // the next one; dropping it here would make the tray flicker items in and out.
                eprintln!("wlrix-tray: could not read {}: {err}", address.service);
            }
        },
        Command::Invoke(address, invocation, x, y) => {
            if let Err(err) = connection.call_method(
                Some(address.service.as_str()),
                address.path.as_str(),
                Some(ITEM_INTERFACE),
                invocation.method(),
                &(x, y),
            ) {
                // Very common and not worth alarm: `SecondaryActivate` in particular is optional,
                // and an item that has not implemented it answers `UnknownMethod`.
                eprintln!(
                    "wlrix-tray: {} on {} did nothing: {err}",
                    invocation.method(),
                    address.service
                );
            }
        }
        Command::Scroll(address, delta, axis) => {
            let _ = connection.call_method(
                Some(address.service.as_str()),
                address.path.as_str(),
                Some(ITEM_INTERFACE),
                "Scroll",
                &(delta, axis.as_str()),
            );
        }
        Command::OpenMenu(address, path) => {
            let menu = dbusmenu::fetch(connection, &address, &path);
            answer(TrayEvent::Menu(address, menu));
        }
        Command::Choose(address, path, id) => {
            if let Err(err) = dbusmenu::choose(connection, &address, &path, id) {
                eprintln!("wlrix-tray: {err}");
            }
        }
        Command::Stop => {}
    }
}
