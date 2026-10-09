// SPDX-License-Identifier: GPL-3.0-or-later
//! Turning an item's `IconName` into pixels.
//!
//! [`wlrix_ui::image::Images`] decodes a *path* at a requested size. Getting from a name to a
//! path is the icon-theme spec -- `index.theme` parsing, theme inheritance, size and scale
//! directories, the `/usr/share/pixmaps` fallback -- so `freedesktop-icons` does that, exactly as
//! `wlrix-desktop/src/icon_theme.rs` does. This module is that one with one addition:
//! `IconThemePath`.
//!
//! # `IconThemePath`
//!
//! An item may name a directory of its own and expect its `IconName` to be looked up *there*
//! first -- it is how a per-input-method or per-account icon reaches a tray at all, since those
//! are in no installed theme. `freedesktop-icons` searches the standard theme directories and
//! cannot be pointed at an arbitrary one, so the private directory is searched by hand before it
//! is consulted: the flat file first, then one level of subdirectories, which is what applications
//! that use this property actually lay out.
//!
//! Only one level. A recursive walk of a directory an application chose is an unbounded amount of
//! work triggered by another process, done while the tray is trying to draw a frame.
//!
//! # Why a theme has to be named
//!
//! `freedesktop_icons::lookup(name)` with no theme searches `hicolor` and `/usr/share/pixmaps`,
//! and almost nothing a tray wants is in either. fcitx5 publishes `IconName =
//! "input-keyboard-symbolic"`, no pixmap and no theme path; that file is in Adwaita on this
//! machine and in every other icon theme, and in hicolor on none of them -- so a bare lookup finds
//! nothing and the tray draws a placeholder next to a perfectly ordinary input method.
//!
//! So the configured theme is searched first and the bare lookup is the fallback behind it. The
//! default is wlrix, the IRIX icon set `wlrix-assets` installs, which inherits Adwaita -- what a
//! GTK application would pick and what every other tray on a Linux desktop is already showing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use wlrix_ui::image::Images;

use crate::pixmap::Pixmap;

/// Extensions worth opening, in the order a theme would prefer them.
const EXTENSIONS: [&str; 3] = ["svg", "png", "xpm"];

/// What a lookup was asked for. The theme path is part of the key: two items may use the same
/// icon name and mean different files.
type Key = (String, String, i32);

/// The name-to-file lookup, and the decoded-image cache behind it.
#[derive(Default)]
pub struct Icons {
    images: Images,
    /// The icon theme to search first. See the module comment.
    theme: String,
    /// Names already resolved to files, including the ones that resolved to nothing.
    resolved: HashMap<(String, String), Option<PathBuf>>,
    /// Decoded and scaled, ready to draw.
    loaded: HashMap<Key, Option<Pixmap>>,
    /// Names already complained about, so a missing icon costs one line rather than one a frame.
    reported: HashMap<String, ()>,
}

impl Icons {
    /// The icon theme to search before falling back to hicolor and `/usr/share/pixmaps`.
    ///
    /// Empty means "no named theme", which is what a user gets by setting `icon_theme = ""`.
    pub fn set_theme(&mut self, theme: &str) {
        if self.theme != theme {
            self.theme = theme.to_owned();
            self.clear();
        }
    }

    /// Find the file `name` refers to, preferring `theme_path` when the item named one.
    ///
    /// An absolute path is taken as-is; anything else is a lookup. `size` is a request, not a
    /// promise -- a theme may only have one size, and the result is scaled to fit.
    pub fn resolve(&mut self, theme_path: &str, name: &str, size: i32) -> Option<PathBuf> {
        if name.is_empty() {
            return None;
        }
        // The path case is not worth caching: it is one `is_file` call.
        let candidate = Path::new(name);
        if candidate.is_absolute() {
            return candidate.is_file().then(|| candidate.to_path_buf());
        }

        let key = (theme_path.to_owned(), name.to_owned());
        if let Some(found) = self.resolved.get(&key) {
            return found.clone();
        }

        let wanted = size.clamp(1, u16::MAX as i32) as u16;
        let found = in_private_directory(theme_path, name)
            .or_else(|| {
                (!self.theme.is_empty()).then(|| {
                    freedesktop_icons::lookup(name)
                        .with_size(wanted)
                        .with_theme(&self.theme)
                        .with_cache()
                        .find()
                })?
            })
            .or_else(|| {
                // No theme named, or the named one does not have it. `hicolor` is the fallback
                // every theme inherits, and the lookup covers `/usr/share/pixmaps` too.
                freedesktop_icons::lookup(name)
                    .with_size(wanted)
                    .with_cache()
                    .find()
            });
        if found.is_none() && self.reported.insert(name.to_owned(), ()).is_none() {
            eprintln!("wlrix-tray: no icon found for {name:?}");
        }
        self.resolved.insert(key, found.clone());
        found
    }

    /// Resolve, decode and scale in one step.
    pub fn get(&mut self, theme_path: &str, name: &str, size: i32) -> Option<&Pixmap> {
        let key = (theme_path.to_owned(), name.to_owned(), size);
        if !self.loaded.contains_key(&key) {
            let decoded = self
                .resolve(theme_path, name, size)
                .and_then(|path| self.images.load(&path, size))
                .map(Pixmap::from_image);
            self.loaded.insert(key.clone(), decoded);
        }
        self.loaded.get(&key)?.as_ref()
    }

    /// Forget everything.
    ///
    /// Called on a config reload, which may have changed the icon size -- that invalidates every
    /// decoded entry -- and when an item says its icon changed, since the *name* may be the same
    /// while the file behind it is not. The negative entries go too, so an icon installed while
    /// the tray was running is found on the next look rather than never.
    pub fn clear(&mut self) {
        self.images.clear();
        self.resolved.clear();
        self.loaded.clear();
        self.reported.clear();
    }
}

/// Look `name` up inside an item's own `IconThemePath`.
///
/// `None` when the item named no directory, when the name is not a plain file name, or when
/// nothing there matches.
fn in_private_directory(theme_path: &str, name: &str) -> Option<PathBuf> {
    if theme_path.is_empty() {
        return None;
    }
    // A name is a bare icon name. Refusing anything with a separator in it keeps an item from
    // reaching out of the directory it named, which it has no business doing.
    if name.contains('/') || name.contains('\\') || name.starts_with('.') {
        return None;
    }
    let root = Path::new(theme_path);
    if !root.is_absolute() {
        return None;
    }

    let matching = |dir: &Path| {
        EXTENSIONS
            .iter()
            .map(|extension| dir.join(format!("{name}.{extension}")))
            .find(|path| path.is_file())
    };
    if let Some(found) = matching(root) {
        return Some(found);
    }
    // One level down, for the `<theme>/<size>/<category>` shapes applications lay out. Sorted,
    // so which of two equally good matches wins does not depend on the order the filesystem
    // happens to hand back.
    let mut children: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    children.sort();
    children.iter().find_map(|dir| matching(dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory laid out the way an item's `IconThemePath` is.
    ///
    /// Named after the caller: these tests run in parallel, and one shared directory means each
    /// one deleting the others' fixtures out from under them.
    fn private_theme(test: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("wlrix-tray-icons-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("22x22")).unwrap();
        std::fs::write(root.join("flat.png"), b"not really a png").unwrap();
        std::fs::write(root.join("22x22").join("nested.svg"), b"<svg/>").unwrap();
        root
    }

    #[test]
    fn a_private_directory_is_searched_before_the_installed_themes() {
        let root = private_theme("searched-first");
        let path = root.to_string_lossy().to_string();
        assert_eq!(
            in_private_directory(&path, "flat"),
            Some(root.join("flat.png"))
        );
        assert_eq!(
            in_private_directory(&path, "nested"),
            Some(root.join("22x22").join("nested.svg")),
            "one level of subdirectory is searched too"
        );
        assert_eq!(in_private_directory(&path, "absent"), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_item_cannot_escape_the_directory_it_named() {
        let root = private_theme("cannot-escape");
        let path = root.to_string_lossy().to_string();
        // The name comes off the bus, from a program the tray does not control.
        for name in ["../../etc/passwd", "sub/thing", ".ssh", "a\\b"] {
            assert_eq!(in_private_directory(&path, name), None, "{name}");
        }
        // And a relative theme path is refused outright, since it would resolve against
        // whatever directory the session happened to start the tray in.
        assert_eq!(in_private_directory("relative/dir", "flat"), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_theme_path_means_straight_to_the_installed_themes() {
        assert_eq!(in_private_directory("", "anything"), None);
    }

    #[test]
    fn an_absolute_icon_name_resolves_to_itself() {
        let root = private_theme("absolute");
        let file = root.join("flat.png");
        let mut icons = Icons::default();
        assert_eq!(
            icons.resolve("", &file.to_string_lossy(), 22),
            Some(file.clone())
        );
        assert_eq!(icons.resolve("", "/nonexistent/icon.png", 22), None);
        assert_eq!(icons.resolve("", "", 22), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
