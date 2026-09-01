// SPDX-License-Identifier: GPL-3.0-or-later
//! wlRIX's application indicator tray.
//!
//! A dock in the corner of the desktop holding the status icons background programs publish over
//! `org.kde.StatusNotifierItem` -- fcitx5's input-method indicator, Steam, and anything else that
//! speaks the protocol. IRIX had one, most often seen holding the wnn indicator; this is that,
//! for the programs a Linux desktop actually runs.
//!
//! Two halves, kept apart on purpose:
//!
//! - [`sni`] is the bus. It owns `org.kde.StatusNotifierWatcher` -- without which no application
//!   publishes anything at all -- and does every read and call on threads of its own.
//! - [`ui`] is the window: one wlr-layer-shell surface, exactly as big as the strip it draws.
//!
//! Between them sit the parts that are neither, and that are therefore testable without a bus or a
//! screen: [`model`] (which items are shown, in what order), [`layout`] (where the cells go),
//! [`menu`] and [`tooltip`] (where a posted menu's rows and a hover tip's lines go), [`pixmap`]
//! and [`icons`] (getting from what an item published to pixels).

pub mod config;
pub mod icons;
pub mod layout;
pub mod menu;
pub mod model;
pub mod pidfile;
pub mod pixmap;
pub mod signals;
pub mod sni;
pub mod tooltip;
pub mod ui;
pub mod xdg;
