// SPDX-License-Identifier: GPL-3.0-or-later
//! The hand-edited settings file.
//!
//! ```toml
//! # ~/.config/wlrix/tray.toml
//! output = "DP-1"              # which monitor gets the tray; default is the leftmost
//! anchor = "bottom-left"       # bottom-left | bottom-right | top-left | top-right
//! orientation = "horizontal"   # horizontal | vertical
//! show_passive = false         # items whose Status is Passive are hidden
//! hide_when_empty = true       # with nothing to show, show nothing at all
//!
//! [appearance]
//! palette = "gotham"           # the color scheme; default is "classic"
//! icon_theme = "Adwaita"       # where an item's IconName is looked up; "" for none
//!
//! [metrics]
//! icon = 22                    # the icon artwork, square
//! cell = 28                    # one cell
//! gap = 2                      # between cells
//! margin = 8                   # between the strip and the screen edges
//! wrap_at = 8                  # cells per run before the strip wraps
//!
//! [[item]]                     # optional per-item overrides, keyed by the item's D-Bus Id
//! id = "fcitx"
//! hidden = false
//! order = 0
//! ```
//!
//! Read from the user's config directory first, then `/etc/wlrix`; the first file found wins
//! outright rather than merging, so what a user sees in their own file is the whole of what they
//! get. This mirrors the compositor, the session and the desktop deliberately: one shape of file
//! across the stack.
//!
//! Unknown keys are an error, for the same reason the compositor rejects them -- a silently
//! ignored typo in a config file is a bad afternoon.
//!
//! `[[item]]` is an **array of tables**, which `wlrix-settings-daemon` deliberately does not
//! manage: a keyed collection needs add/remove/reorder rather than get/set. A settings app can
//! read and write the scalars above; the item overrides stay hand-edited for now.

use serde::Deserialize;

use crate::layout::{Anchor, Metrics, Orientation};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Which monitor the tray appears on, by connector name (`DP-1`, `Virtual-1`, ...).
    /// Absent means "work it out" -- see [`crate::ui`].
    #[serde(default)]
    pub output: Option<String>,
    /// Which corner it docks in. IRIX put it bottom-left, which is the default.
    #[serde(default)]
    pub anchor: Anchor,
    /// Which way the strip runs from that corner.
    #[serde(default)]
    pub orientation: Orientation,
    /// Whether to show items whose `Status` is `Passive`.
    ///
    /// The spec says a Passive item "is not of pressing importance to the user" and may be
    /// hidden, and applications rely on that -- several park an item in Passive permanently and
    /// only ever raise it to Active when they have something to say. Showing them all turns the
    /// tray into a list of everything that has ever started, so the default is off.
    #[serde(default)]
    pub show_passive: bool,
    /// Whether an empty tray disappears entirely, rather than leaving an empty well.
    ///
    /// Defaults to on, which is why it is stored inverted -- `bool`'s `Default` is `false`, and
    /// a field that has to say `#[serde(default = "…")]` to mean "yes" reads badly in a diff.
    /// [`Config::hide_when_empty`] is the accessor everything else uses.
    #[serde(default, rename = "hide_when_empty")]
    hide_when_empty: Option<bool>,
    #[serde(default)]
    pub appearance: AppearanceConfig,
    #[serde(default)]
    pub metrics: MetricsConfig,
    /// Per-item overrides, keyed by the item's D-Bus `Id`.
    #[serde(default)]
    pub item: Vec<ItemConfig>,
}

/// Which color scheme to draw in.
///
/// Its own section rather than a bare key, because a scheme is not the only thing that will ever
/// go here -- and because it is the same section name the compositor and the desktop use, so the
/// three files read alike.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppearanceConfig {
    /// A scheme id from `wlrix-ui`: `classic`, `classic-g10`, `classic-g24`, `gotham`. Absent,
    /// empty, or unrecognized means the default, with a line in the log for the last of those --
    /// a mistyped scheme name must not leave the tray unpainted.
    #[serde(default)]
    pub palette: Option<String>,
    /// The icon theme an item's `IconName` is looked up in, before hicolor and
    /// `/usr/share/pixmaps`.
    ///
    /// Defaults to Adwaita, and that default earns its keep: fcitx5 publishes
    /// `IconName = "input-keyboard-symbolic"` with no pixmap and no theme path, and that file is
    /// in every icon theme *except* hicolor -- so without a named theme the input-method indicator
    /// draws as a placeholder. See [`crate::icons`].
    ///
    /// An empty string means "no named theme", for a machine whose themes are all wrong for a
    /// 22-pixel cell.
    #[serde(default)]
    pub icon_theme: Option<String>,
}

/// What [`AppearanceConfig::icon_theme`] means when the file does not say.
const DEFAULT_ICON_THEME: &str = "Adwaita";

impl AppearanceConfig {
    /// The icon theme to search first.
    pub fn icon_theme(&self) -> &str {
        self.icon_theme.as_deref().unwrap_or(DEFAULT_ICON_THEME)
    }
}

/// What to do with one particular item.
///
/// Keyed by the item's `Id` property -- `fcitx`, `steam`, ... -- and *not* by its bus name, which
/// is a different unique name on every run.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemConfig {
    pub id: String,
    /// Keep this item out of the tray entirely, whatever its status.
    #[serde(default)]
    pub hidden: bool,
    /// Where it sorts. Items with an `order` come first, lowest first; the rest follow in the
    /// order they registered, which is roughly the order their programs started.
    #[serde(default)]
    pub order: Option<i32>,
}

/// Cell geometry. Every field is optional and falls back to [`Metrics::default`], so a file that
/// only wants bigger icons says only that.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsConfig {
    pub icon: Option<i32>,
    pub cell: Option<i32>,
    pub gap: Option<i32>,
    pub margin: Option<i32>,
    /// How many cells a run holds before the strip wraps. Named for what it does rather than
    /// `max_columns`, because in `vertical` orientation the run is a column and the wrap makes a
    /// new one sideways -- a key called `max_columns` that caps *rows* is a key nobody can use.
    pub wrap_at: Option<i32>,
}

impl MetricsConfig {
    /// The configured metrics, with anything unset left at the default.
    ///
    /// Values are floored at sane minimums rather than rejected: a cell smaller than its icon, or
    /// a negative gap, would put the layout arithmetic somewhere it cannot recover from, and
    /// refusing to start over a silly number in a config file helps nobody.
    pub fn resolve(&self) -> Metrics {
        let base = Metrics::default();
        let icon = self.icon.unwrap_or(base.icon).max(8);
        Metrics {
            icon,
            cell: self.cell.unwrap_or(base.cell).max(icon),
            gap: self.gap.unwrap_or(base.gap).max(0),
            margin: self.margin.unwrap_or(base.margin).max(0),
            wrap_at: self.wrap_at.unwrap_or(base.wrap_at).max(1),
        }
    }
}

impl Config {
    /// Whether an empty tray hides itself. Defaults to yes.
    pub fn hide_when_empty(&self) -> bool {
        self.hide_when_empty.unwrap_or(true)
    }

    /// The override for one item's `Id`, if the file names it.
    pub fn item(&self, id: &str) -> Option<&ItemConfig> {
        self.item.iter().find(|item| item.id == id)
    }

    /// Load the first config file that exists. No file at all is not an error -- the defaults are
    /// a working tray.
    pub fn load() -> Self {
        for path in crate::xdg::config_paths() {
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                // Not-found is the ordinary case; only real errors are worth a line.
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => {
                    eprintln!("wlrix-tray: could not read {}: {err}", path.display());
                    continue;
                }
            };
            match toml::from_str::<Self>(&text) {
                Ok(config) => return config,
                Err(err) => {
                    // Loud, then carry on with defaults: a broken config should not cost the user
                    // their tray, and a tray that is not running is a fcitx5 with no indicator.
                    eprintln!("wlrix-tray: {} is not valid: {err}", path.display());
                    return Self::default();
                }
            }
        }
        Self::default()
    }
}

/// Parse a candidate config file, for `--check-config`.
///
/// This program's own serde types are the authority on what `tray.toml` may contain.
/// `wlrix-settings-daemon` writes a temporary file and runs this against it before renaming it
/// into place, so a settings app cannot produce a file this program would refuse -- which matters
/// because `deny_unknown_fields` means one wrong key costs the *whole* file and the user silently
/// gets built-in defaults for all of it.
///
/// Deliberately not [`Config::load`]: that reports to stderr and carries on with defaults, which
/// is right at startup -- a typo should cost the setting, not the tray -- and exactly wrong here,
/// where the question *is* whether the file is acceptable.
pub fn check(path: &std::path::Path) -> Result<(), String> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("could not read {}: {err}", path.display()))?;
    toml::from_str::<Config>(&text)
        .map(|_| ())
        .map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> Config {
        toml::from_str(text).expect("the test config should parse")
    }

    #[test]
    fn an_empty_file_is_all_defaults() {
        let config: Config = toml::from_str("").expect("empty config should parse");
        assert_eq!(config.output, None);
        assert_eq!(config.anchor, Anchor::BottomLeft);
        assert_eq!(config.orientation, Orientation::Horizontal);
        assert!(!config.show_passive);
        assert!(config.hide_when_empty());
        assert_eq!(config.metrics.resolve(), Metrics::default());
    }

    #[test]
    fn metrics_override_one_field_at_a_time() {
        let config: Config = toml::from_str("[metrics]\nicon = 32\n").unwrap();
        let metrics = config.metrics.resolve();
        assert_eq!(metrics.icon, 32);
        assert_eq!(metrics.gap, Metrics::default().gap);
        assert_eq!(metrics.wrap_at, Metrics::default().wrap_at);
    }

    #[test]
    fn a_typo_is_refused_rather_than_ignored() {
        // `show_pasive` would otherwise silently do nothing, and the user would never find out
        // why their setting had no effect.
        assert!(toml::from_str::<Config>("show_pasive = true\n").is_err());
        assert!(toml::from_str::<Config>("[metrics]\nmax_columns = 4\n").is_err());
    }

    #[test]
    fn a_misspelled_corner_is_refused_too() {
        // Not warn-and-default, unlike the palette: an unrecognized *scheme* still leaves a
        // readable tray, whereas silently ignoring `anchor` puts the whole thing in the wrong
        // corner with nothing said. serde's error names the four it will take.
        let err = toml::from_str::<Config>("anchor = \"bottom left\"\n").unwrap_err();
        assert!(err.to_string().contains("bottom-left"), "{err}");
    }

    #[test]
    fn nonsense_geometry_is_clamped_not_fatal() {
        let config: Config =
            toml::from_str("[metrics]\nicon = -10\ncell = 1\ngap = -5\nmargin = -5\nwrap_at = 0\n")
                .unwrap();
        let metrics = config.metrics.resolve();
        assert!(metrics.icon >= 8);
        assert!(metrics.cell >= metrics.icon, "a cell must hold its icon");
        assert!(metrics.gap >= 0);
        assert!(metrics.margin >= 0);
        assert!(metrics.wrap_at >= 1, "a run of zero cells never wraps");
    }

    #[test]
    fn item_overrides_are_looked_up_by_id() {
        let config: Config = toml::from_str(
            "[[item]]\nid = \"fcitx\"\norder = 0\n\n\
             [[item]]\nid = \"steam\"\nhidden = true\n",
        )
        .unwrap();
        assert_eq!(config.item("fcitx").unwrap().order, Some(0));
        assert!(config.item("steam").unwrap().hidden);
        assert!(config.item("nothing-like-this").is_none());
    }

    #[test]
    fn the_icon_theme_defaults_to_something_that_has_the_icons() {
        // Not hicolor: fcitx5's `input-keyboard-symbolic` is in every theme except that one, so
        // an empty default would draw a placeholder for the commonest item there is.
        assert_eq!(config("").appearance.icon_theme(), "Adwaita");
        assert_eq!(
            config("[appearance]\nicon_theme = \"hicolor\"\n")
                .appearance
                .icon_theme(),
            "hicolor"
        );
        // ...and it can be turned off outright.
        assert_eq!(
            config("[appearance]\nicon_theme = \"\"\n")
                .appearance
                .icon_theme(),
            ""
        );
    }

    #[test]
    fn a_full_file_round_trips() {
        let config: Config = toml::from_str(
            "output = \"DP-1\"\n\
             anchor = \"top-right\"\n\
             orientation = \"vertical\"\n\
             show_passive = true\n\
             hide_when_empty = false\n\
             [appearance]\n\
             palette = \"gotham\"\n\
             [metrics]\n\
             icon = 24\n\
             cell = 30\n\
             gap = 3\n\
             margin = 12\n\
             wrap_at = 5\n",
        )
        .unwrap();
        assert_eq!(config.output.as_deref(), Some("DP-1"));
        assert_eq!(config.anchor, Anchor::TopRight);
        assert_eq!(config.orientation, Orientation::Vertical);
        assert!(config.show_passive);
        assert!(!config.hide_when_empty());
        assert_eq!(config.appearance.palette.as_deref(), Some("gotham"));
        assert_eq!(
            config.metrics.resolve(),
            Metrics {
                icon: 24,
                cell: 30,
                gap: 3,
                margin: 12,
                wrap_at: 5,
            }
        );
    }
}
