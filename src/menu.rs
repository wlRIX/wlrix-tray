// SPDX-License-Identifier: GPL-3.0-or-later
//! A posted item menu: where its rows are, and which one the pointer is on.
//!
//! The geometry is `wlrix-desktop/src/menu.rs`'s, constant for constant, which is
//! `wlrix-compositor/src/menu.rs`'s in turn. The three are the same object as far as the user is
//! concerned -- right-clicking the desktop, a titlebar, or a tray icon should not produce three
//! menus that differ by a pixel -- so the numbers are shared by being identical and commented as
//! such, in the same way those two already are.
//!
//! What is new here is **cascading**. The desktop menu is one flat panel; an item's menu has
//! submenus (fcitx5 puts its input-method list in one), so a posted menu is a *chain* of columns:
//! the root, plus one for each open submenu. They are laid out together and measured together,
//! because the surface has to be big enough for all of them at once -- see [`crate::layout`] for
//! why the surface is exactly the size of what it draws.
//!
//! Menu-local coordinates throughout: the root column's top-left is the origin. Where the block
//! goes on screen is [`crate::layout::Frame`]'s business.

use crate::sni::dbusmenu::{Entry, Menu};

pub use wlrix_ui::canvas::Rect;

/// Height of an ordinary item row.
const ITEM_H: i32 = 22;
/// Height of a separator row.
const SEPARATOR_H: i32 = 7;
/// Height of the title row at the top of the root column.
const HEADER_H: i32 = 24;
/// The narrowest a column gets.
///
/// Not `wlrix-desktop`'s 186: that is sized for "Change Permissions", a label this menu never
/// has. An item's rows are short -- "Mozc", "Restart", "Exit" -- and a floor wide enough for the
/// desktop's longest fixed label would leave every tray menu two-thirds empty.
const WIDTH: i32 = 140;
/// Margin between the panel edge and the rows.
const MARGIN: i32 = 3;
/// Left inset of an item's label. The check and radio indicators are drawn inside it.
pub const LABEL_INSET: i32 = 14;
/// Label size in logical pixels.
pub const LABEL_PX: f32 = 14.0;
/// Bevel thickness of the panel and of a highlighted row.
pub const BEVEL: i32 = 2;
/// Room reserved at the right of a row for the submenu arrow.
pub const ARROW_W: i32 = 12;
/// How far a submenu column overlaps its parent, so the two read as attached.
///
/// Wider than [`MARGIN`] on purpose: at exactly the margin the submenu's rows would begin on the
/// parent's outer edge and the two panels would read as merely adjacent. Overlapping by more tucks
/// the cascade under the parent's bevel, which is what makes it look attached -- and it is what
/// puts a submenu row genuinely over a parent row, which is the case [`Posted::at`] has to get
/// right.
const CASCADE_OVERLAP: i32 = 6;

/// One row of a laid-out column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// The item's name at the top of the root column, with a groove under it.
    Header,
    Separator,
    /// A row of the menu, by its index in that column's entries.
    Item(usize),
}

/// One column of the cascade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    /// The panel, bevel included, in menu-local coordinates.
    pub panel: Rect,
    /// The rows, in order, each with the rectangle it occupies.
    pub rows: Vec<(Row, Rect)>,
}

/// Where the pointer is, as something the caller can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub column: usize,
    /// Index into that column's entries.
    pub entry: usize,
}

/// A menu on screen.
pub struct Posted {
    /// The item this belongs to, so a chosen row goes back to the right object.
    pub menu: Menu,
    /// The item's name, drawn as the header.
    header: String,
    /// Which submenu is open at each level: `[2]` means "the third row of the root column".
    open: Vec<usize>,
    /// The highlighted row.
    hovered: Option<Hit>,
    columns: Vec<Column>,
}

impl Posted {
    /// Lay a menu out with nothing expanded.
    ///
    /// `measure` gives the width of a label. Passed in rather than measured here so this module
    /// stays free of font machinery and its hit-testing stays a matter of arithmetic -- and so a
    /// test can size a menu without loading a font. The same arrangement `wlrix-desktop` uses.
    pub fn new(menu: Menu, header: &str, measure: impl FnMut(&str) -> i32) -> Self {
        let mut posted = Self {
            menu,
            header: header.to_owned(),
            open: Vec::new(),
            hovered: None,
            columns: Vec::new(),
        };
        posted.relayout(measure);
        posted
    }

    /// The entries of one column, following the open chain.
    pub fn entries(&self, column: usize) -> &[Entry] {
        let mut entries = self.menu.entries.as_slice();
        for &index in self.open.iter().take(column) {
            match entries.get(index) {
                Some(entry) => entries = entry.children.as_slice(),
                // Only reachable if the tree changed under an open menu, which
                // `TrayEvent::MenuStale` closes -- but answering with nothing beats indexing
                // past the end of a menu the user is pointing at.
                None => return &[],
            }
        }
        entries
    }

    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    /// The item's name, drawn as the root column's header.
    ///
    /// Owned by the menu rather than passed to the painter, so the title that is drawn and the
    /// title the column was measured against cannot end up different -- which would show up as a
    /// header running past the panel edge.
    pub fn header(&self) -> &str {
        &self.header
    }

    pub fn hovered(&self) -> Option<Hit> {
        self.hovered
    }

    /// How big the whole cascade is, so the surface can be sized for it.
    pub fn size(&self) -> (i32, i32) {
        let right = self
            .columns
            .iter()
            .map(|column| column.panel.right())
            .max()
            .unwrap_or(0);
        let bottom = self
            .columns
            .iter()
            .map(|column| column.panel.bottom())
            .max()
            .unwrap_or(0);
        (right, bottom)
    }

    /// Whether a point in menu-local coordinates is on the menu at all.
    ///
    /// A press anywhere else dismisses it. Note that this is *not* the whole surface: the surface
    /// is the union of the menu and the strip, and the corner between them belongs to neither.
    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.columns
            .iter()
            .any(|column| column.panel.contains(x, y))
    }

    /// The row under a point.
    pub fn at(&self, x: i32, y: i32) -> Option<Hit> {
        // Deepest column first: a submenu overlaps its parent, and the row underneath must not
        // win over the panel drawn on top of it.
        for (index, column) in self.columns.iter().enumerate().rev() {
            if !column.panel.contains(x, y) {
                continue;
            }
            return column.rows.iter().find_map(|(row, rect)| match row {
                Row::Item(entry) if rect.contains(x, y) => Some(Hit {
                    column: index,
                    entry: *entry,
                }),
                _ => None,
            });
        }
        None
    }

    /// Track the pointer: highlight the row under it, and open or close submenus to match.
    ///
    /// Returns whether anything changed, so the caller can avoid redrawing for nothing.
    pub fn hover(&mut self, x: i32, y: i32, measure: impl FnMut(&str) -> i32) -> bool {
        let hit = self.at(x, y);
        let mut open = self.open.clone();
        if let Some(hit) = hit {
            // Everything deeper than the column being pointed at closes; a submenu the pointer
            // has left should not stay on screen.
            open.truncate(hit.column);
            if self
                .entries(hit.column)
                .get(hit.entry)
                .is_some_and(|entry| entry.submenu)
            {
                open.push(hit.entry);
            }
        }
        // Nothing under the pointer leaves the cascade as it was: crossing the gap between a
        // parent row and its submenu passes over the panel edge, and closing there would make a
        // submenu impossible to reach.
        let changed = hit != self.hovered || open != self.open;
        self.hovered = hit;
        if open != self.open {
            self.open = open;
            self.relayout(measure);
        }
        changed
    }

    /// Stop highlighting anything, without closing what is open.
    pub fn unhover(&mut self) -> bool {
        let changed = self.hovered.is_some();
        self.hovered = None;
        changed
    }

    /// The id to send back to the application, if the pointer is on a row that can be chosen.
    pub fn choice(&self) -> Option<i32> {
        let hit = self.hovered?;
        let entry = self.entries(hit.column).get(hit.entry)?;
        entry.selectable().then_some(entry.id)
    }

    /// Rebuild the column rectangles for the current open chain.
    fn relayout(&mut self, mut measure: impl FnMut(&str) -> i32) {
        let mut columns = Vec::new();
        let mut origin = (0, 0);
        let depth = self.open.len();

        for level in 0..=depth {
            let entries = self.entries(level);
            let header = (level == 0).then_some(self.header.as_str());
            let width = width_for(entries, header, &mut measure);
            let column = lay_out(entries, header, origin, width);

            // A submenu hangs off the row that opened it, overlapping its parent slightly so the
            // two read as one object -- and never quite level with it, since a submenu whose
            // first row sits exactly on its parent row looks like the parent moved.
            if let Some(&index) = self.open.get(level) {
                let parent_row = column
                    .rows
                    .iter()
                    .find(|(row, _)| *row == Row::Item(index))
                    .map(|(_, rect)| *rect)
                    .unwrap_or(column.panel);
                origin = (
                    column.panel.right() - CASCADE_OVERLAP,
                    parent_row.y - MARGIN,
                );
            }
            columns.push(column);
        }
        self.columns = columns;
    }
}

/// How wide a column has to be for its rows.
///
/// The inset is counted on both sides so a long label does not sit flush against the right bevel,
/// and the arrow gets its own room so a submenu label never runs underneath it.
fn width_for(
    entries: &[Entry],
    header: Option<&str>,
    measure: &mut impl FnMut(&str) -> i32,
) -> i32 {
    // A plain loop rather than an iterator chain: two closures over the same `&mut` measure
    // cannot coexist, and `Fonts` measures mutably because it caches its shaping as it goes.
    let mut widest = header.map_or(0, &mut *measure);
    for entry in entries {
        let arrow = if entry.submenu { ARROW_W } else { 0 };
        widest = widest.max(measure(&entry.label) + arrow);
    }
    WIDTH.max(widest + 2 * (MARGIN + LABEL_INSET))
}

/// Place the rows of one column.
fn lay_out(entries: &[Entry], header: Option<&str>, origin: (i32, i32), width: i32) -> Column {
    let mut rows = Vec::new();
    let mut top = origin.1 + MARGIN;
    let row_width = width - 2 * MARGIN;

    if header.is_some() {
        rows.push((
            Row::Header,
            Rect::new(origin.0 + MARGIN, top, row_width, HEADER_H),
        ));
        top += HEADER_H;
    }
    for (index, entry) in entries.iter().enumerate() {
        let (kind, height) = if entry.separator {
            (Row::Separator, SEPARATOR_H)
        } else {
            (Row::Item(index), ITEM_H)
        };
        rows.push((kind, Rect::new(origin.0 + MARGIN, top, row_width, height)));
        top += height;
    }

    Column {
        panel: Rect::new(origin.0, origin.1, width, top - origin.1 + MARGIN),
        rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sni::dbusmenu::{Menu, Toggle};

    /// A stand-in for the font: a fixed width per character. Narrow enough that the short labels
    /// used here all fit inside [`WIDTH`], so the menus tested are laid out as real ones are.
    fn measure(label: &str) -> i32 {
        label.chars().count() as i32 * 8
    }

    fn entry(id: i32, label: &str) -> Entry {
        Entry {
            id,
            label: label.to_owned(),
            enabled: true,
            separator: false,
            toggle: Toggle::None,
            icon_name: String::new(),
            children: Vec::new(),
            submenu: false,
        }
    }

    fn separator(id: i32) -> Entry {
        Entry {
            separator: true,
            ..entry(id, "")
        }
    }

    /// fcitx5's menu, near enough: a submenu of input methods, a separator, and two actions.
    fn fcitx_menu() -> Menu {
        let mut methods = entry(1, "Input Method");
        methods.submenu = true;
        methods.children = vec![
            Entry {
                toggle: Toggle::Radio(Some(true)),
                ..entry(10, "Mozc")
            },
            Entry {
                toggle: Toggle::Radio(Some(false)),
                ..entry(11, "Keyboard")
            },
        ];
        Menu {
            path: "/MenuBar".into(),
            entries: vec![methods, separator(2), entry(3, "Restart"), entry(4, "Exit")],
        }
    }

    fn posted() -> Posted {
        Posted::new(fcitx_menu(), "Fcitx", measure)
    }

    /// The middle of a row of the given column.
    fn middle(posted: &Posted, column: usize, row: Row) -> (i32, i32) {
        let rect = posted.columns()[column]
            .rows
            .iter()
            .find(|(kind, _)| *kind == row)
            .expect("that row should exist")
            .1;
        (rect.x + rect.w / 2, rect.y + rect.h / 2)
    }

    #[test]
    fn a_fresh_menu_is_one_column_with_a_header() {
        let posted = posted();
        assert_eq!(posted.columns().len(), 1);
        assert_eq!(posted.columns()[0].rows[0].0, Row::Header);
        // Header, submenu row, separator, and two actions.
        assert_eq!(posted.columns()[0].rows.len(), 5);
        assert_eq!(posted.size().0, posted.columns()[0].panel.w);
    }

    #[test]
    fn a_separator_is_shorter_than_an_item_and_cannot_be_hit() {
        let posted = posted();
        let heights: Vec<i32> = posted.columns()[0]
            .rows
            .iter()
            .map(|(_, rect)| rect.h)
            .collect();
        assert_eq!(heights, vec![HEADER_H, ITEM_H, SEPARATOR_H, ITEM_H, ITEM_H]);
        let (x, y) = middle(&posted, 0, Row::Separator);
        assert_eq!(posted.at(x, y), None);
    }

    #[test]
    fn hovering_a_submenu_row_opens_it_and_grows_the_menu() {
        let mut posted = posted();
        let narrow = posted.size();
        let (x, y) = middle(&posted, 0, Row::Item(0));
        assert!(posted.hover(x, y, measure));
        assert_eq!(posted.columns().len(), 2, "the cascade opened");
        assert!(posted.size().0 > narrow.0, "and the surface has to grow");
        // The submenu hangs off its parent row rather than starting at the top of the panel.
        assert!(posted.columns()[1].panel.x > posted.columns()[0].panel.x);
        // A submenu row is not itself a choice.
        assert_eq!(posted.choice(), None);
    }

    #[test]
    fn a_row_inside_an_open_submenu_is_the_one_chosen() {
        let mut posted = posted();
        let (x, y) = middle(&posted, 0, Row::Item(0));
        posted.hover(x, y, measure);
        let (x, y) = middle(&posted, 1, Row::Item(0));
        posted.hover(x, y, measure);
        assert_eq!(posted.hovered().map(|hit| hit.column), Some(1));
        assert_eq!(posted.choice(), Some(10), "Mozc's id, not its parent's");
    }

    #[test]
    fn a_submenu_wins_over_the_parent_row_it_covers() {
        // The cascade overlaps its parent by a few pixels. Testing the parent first would give
        // the row underneath, and the user would choose a menu item by pointing at another one.
        let mut posted = posted();
        let (x, y) = middle(&posted, 0, Row::Item(0));
        posted.hover(x, y, measure);
        let overlap = posted.columns()[1].panel;
        let (x, y) = (overlap.x + MARGIN + 1, overlap.y + MARGIN + 1);
        assert!(
            x < posted.columns()[0].panel.right(),
            "the test point has to be over the parent panel for this to prove anything"
        );
        assert_eq!(posted.at(x, y).map(|hit| hit.column), Some(1));
    }

    #[test]
    fn pointing_at_another_root_row_closes_the_submenu() {
        let mut posted = posted();
        let (x, y) = middle(&posted, 0, Row::Item(0));
        posted.hover(x, y, measure);
        assert_eq!(posted.columns().len(), 2);
        let (x, y) = middle(&posted, 0, Row::Item(2));
        posted.hover(x, y, measure);
        assert_eq!(posted.columns().len(), 1);
        assert_eq!(posted.choice(), Some(3), "Restart");
    }

    #[test]
    fn crossing_the_gap_to_a_submenu_does_not_close_it() {
        // Between a parent row and its submenu the pointer passes over panel edges that are on
        // no row at all. Closing there would make a cascading menu impossible to use.
        let mut posted = posted();
        let (x, y) = middle(&posted, 0, Row::Item(0));
        posted.hover(x, y, measure);
        let panel = posted.columns()[1].panel;
        posted.hover(panel.x, panel.y, measure);
        assert_eq!(posted.columns().len(), 2, "the submenu stayed open");
    }

    #[test]
    fn a_disabled_row_highlights_but_cannot_be_chosen() {
        let menu = Menu {
            path: "/MenuBar".into(),
            entries: vec![Entry {
                enabled: false,
                ..entry(5, "Busy")
            }],
        };
        let mut posted = Posted::new(menu, "Item", measure);
        let (x, y) = middle(&posted, 0, Row::Item(0));
        posted.hover(x, y, measure);
        assert!(posted.hovered().is_some());
        assert_eq!(posted.choice(), None);
    }

    #[test]
    fn a_long_label_widens_the_column_rather_than_being_cut() {
        // Labels come out of an application, in the user's language. "Restart" is no bound.
        let menu = Menu {
            path: "/MenuBar".into(),
            entries: vec![entry(
                1,
                "Configure Input Method Engines and Everything Else",
            )],
        };
        let posted = Posted::new(menu, "Item", measure);
        assert!(posted.columns()[0].panel.w > WIDTH);
        let row = posted.columns()[0].rows[1].1;
        assert!(row.w >= measure("Configure Input Method Engines and Everything Else"));
    }

    #[test]
    fn a_point_off_the_menu_is_on_nothing() {
        let posted = posted();
        assert!(!posted.contains(-1, -1));
        assert!(posted.contains(1, 1));
        assert_eq!(posted.at(-1, -1), None);
    }
}
