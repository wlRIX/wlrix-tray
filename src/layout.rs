// SPDX-License-Identifier: GPL-3.0-or-later
//! Where the strip goes, how big it is, and which cell the pointer is over.
//!
//! Pure arithmetic: no Wayland, no D-Bus, no drawing. That is deliberate, because this is the
//! part that has to be right for four different corners and two orientations and there is no way
//! to eyeball all eight on a screen.
//!
//! # The anchored frame
//!
//! A layer surface anchored to a corner keeps that corner still when it is resized, so the tray
//! reasons in **anchored coordinates**: the origin is the anchored corner, `x` runs along the
//! screen edge away from it and `y` runs inward. In those coordinates the strip is always at the
//! origin and always grows the same way, whichever corner the user picked. [`Frame::place`]
//! converts back to the surface's own top-left-origin coordinates at the end, once.
//!
//! This is why cells wrap *away* from the anchored edge: a second row of a bottom-left tray
//! appears above the first, not below it, so the strip grows into the desktop rather than off
//! the screen. It is the same idea as `wlrix-desktop`'s icon grid growing away from the
//! compositor's minimized-window grid -- two things that fill a corner should fill it in
//! opposite directions.

use serde::Deserialize;

pub use wlrix_ui::canvas::Rect;

/// Bevel thickness of the strip's surrounding well, matching `wlrix-desktop`'s menu panel.
pub const BEVEL: i32 = 2;
/// Padding between that bevel and the outermost cell.
pub const PAD: i32 = 2;
/// Gap between the strip and a menu posted from it.
pub const MENU_GAP: i32 = 2;

/// Which corner the tray is docked in.
///
/// IRIX put it bottom-left, which is the default; the other three are the same arithmetic with a
/// flipped axis, so they cost a `match` rather than a feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Anchor {
    #[default]
    BottomLeft,
    BottomRight,
    TopLeft,
    TopRight,
}

impl Anchor {
    /// Whether the anchored `x` axis runs leftward across the screen.
    fn flip_x(self) -> bool {
        matches!(self, Anchor::BottomRight | Anchor::TopRight)
    }

    /// Whether the anchored `y` axis runs upward across the screen.
    fn flip_y(self) -> bool {
        matches!(self, Anchor::BottomLeft | Anchor::BottomRight)
    }
}

/// Which way the strip runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Orientation {
    /// Cells run along the screen edge; a full run wraps to a second row inward.
    #[default]
    Horizontal,
    /// Cells run inward from the edge; a full run wraps to a second column sideways.
    Vertical,
}

/// Cell geometry, already resolved and clamped. See [`crate::config::MetricsConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    /// The icon artwork, square.
    pub icon: i32,
    /// One cell: the icon plus the room its bevel and highlight need.
    pub cell: i32,
    /// Between cells.
    pub gap: i32,
    /// Between the strip and the two screen edges it is anchored to.
    pub margin: i32,
    /// How many cells a run holds before the strip wraps.
    pub wrap_at: i32,
}

impl Default for Metrics {
    /// IRIX's tray was small -- an indicator, not a launcher. 22 is also the size almost every
    /// `IconPixmap` on the bus arrives at, so it is the one size that needs no scaling.
    fn default() -> Self {
        Self {
            icon: 22,
            cell: 28,
            gap: 2,
            margin: 8,
            wrap_at: 8,
        }
    }
}

/// How many cells fit along the run, and how many runs there are.
///
/// Split out because both the frame and the tests want it, and because getting the ceiling
/// division the wrong way round is the classic way to lose the last item off the end.
fn runs(count: i32, wrap_at: i32) -> (i32, i32) {
    let wrap_at = wrap_at.max(1);
    let along = count.min(wrap_at).max(1);
    // Written out rather than `div_ceil`, which is stable only for the unsigned integers.
    let across = ((count + wrap_at - 1) / wrap_at).max(1);
    (along, across)
}

/// The whole surface for one frame: how big it is, and where the pieces sit inside it.
///
/// The strip rectangle is the well, bevel included. Cell rectangles are inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: i32,
    pub height: i32,
    /// The strip's well, in surface coordinates.
    pub strip: Rect,
    /// The posted menu or tooltip, in surface coordinates, when one is showing.
    ///
    /// One slot for both because they are never on screen together: a tooltip explains what a
    /// cell is, and once the menu is open the user has stopped asking.
    pub popup: Option<Rect>,
    anchor: Anchor,
    orientation: Orientation,
    metrics: Metrics,
    count: i32,
}

impl Frame {
    /// Lay out `count` visible cells, with an optional popup of `popup` size shown from the cell
    /// at `from`.
    ///
    /// `bounds` is the output's logical size, used only to stop a popup running off the far edge.
    /// `None` means "not known yet", which happens for exactly one frame at startup.
    pub fn new(
        count: usize,
        anchor: Anchor,
        orientation: Orientation,
        metrics: Metrics,
        popup: Option<(i32, i32)>,
        from: usize,
        bounds: Option<(i32, i32)>,
    ) -> Self {
        let count = count as i32;
        let (along, across) = runs(count, metrics.wrap_at);
        let (cols, rows) = match orientation {
            Orientation::Horizontal => (along, across),
            Orientation::Vertical => (across, along),
        };

        let span = |n: i32| n * metrics.cell + (n - 1).max(0) * metrics.gap;
        let inset = BEVEL + PAD;
        let strip_w = span(cols) + 2 * inset;
        let strip_h = span(rows) + 2 * inset;

        // In anchored coordinates the strip is always at the origin, so the popup is placed
        // relative to a corner that does not move -- which is the whole reason for the change of
        // basis. It sits just inward of the strip, aligned with the cell it came from.
        let placed = popup.map(|(menu_w, menu_h)| {
            let cell = cell_in_strip(from as i32, cols, rows, orientation, metrics);
            let (mut menu_x, mut menu_y) = match orientation {
                Orientation::Horizontal => (cell.x, strip_h + MENU_GAP),
                Orientation::Vertical => (strip_w + MENU_GAP, cell.y),
            };
            // Do not let it run off the far end of the screen. The near end is the anchored
            // corner, which it cannot reach: anchored coordinates only go outward from there.
            if let Some((bound_w, bound_h)) = bounds {
                let limit = |pos: i32, size: i32, bound: i32| {
                    (pos.min(bound - metrics.margin - size)).max(0)
                };
                menu_x = limit(menu_x, menu_w, bound_w);
                menu_y = limit(menu_y, menu_h, bound_h);
            }
            Rect::new(menu_x, menu_y, menu_w, menu_h)
        });

        // The surface is exactly the union of what is drawn. Any slack would be a transparent
        // margin, and a transparent margin on the bottom layer swallows clicks that belong to
        // `wlrix-desktop` -- the compositor's `layer_under` picks the topmost bottom-layer surface
        // by bounding box and, when its input region rejects the point, falls through to the
        // *background* layer rather than to the desktop underneath.
        let width = placed.map_or(strip_w, |menu| strip_w.max(menu.right()));
        let height = placed.map_or(strip_h, |menu| strip_h.max(menu.bottom()));

        let flip_x = anchor.flip_x();
        let flip_y = anchor.flip_y();
        let to_surface = |rect: Rect| {
            Rect::new(
                if flip_x {
                    width - rect.x - rect.w
                } else {
                    rect.x
                },
                if flip_y {
                    height - rect.y - rect.h
                } else {
                    rect.y
                },
                rect.w,
                rect.h,
            )
        };

        Self {
            width,
            height,
            strip: to_surface(Rect::new(0, 0, strip_w, strip_h)),
            popup: placed.map(to_surface),
            anchor,
            orientation,
            metrics,
            count,
        }
    }

    /// The cell at `index`, in surface coordinates. `None` past the end.
    pub fn cell(&self, index: usize) -> Option<Rect> {
        let index = index as i32;
        if index < 0 || index >= self.count {
            return None;
        }
        let (along, across) = runs(self.count, self.metrics.wrap_at);
        let (cols, rows) = match self.orientation {
            Orientation::Horizontal => (along, across),
            Orientation::Vertical => (across, along),
        };
        let local = cell_in_strip(index, cols, rows, self.orientation, self.metrics);
        // Cells are positioned inside the strip in anchored coordinates too, so they flip with
        // it -- and the flip is applied to the *strip's* box, not the surface's, or a wrapped
        // second row would land on the wrong side of a menu.
        let x = if self.anchor.flip_x() {
            self.strip.w - local.x - local.w
        } else {
            local.x
        };
        let y = if self.anchor.flip_y() {
            self.strip.h - local.y - local.h
        } else {
            local.y
        };
        Some(Rect::new(
            self.strip.x + x,
            self.strip.y + y,
            local.w,
            local.h,
        ))
    }

    /// Which cell covers a point in surface coordinates.
    pub fn cell_at(&self, x: i32, y: i32) -> Option<usize> {
        (0..self.count as usize)
            .find(|&index| self.cell(index).is_some_and(|cell| cell.contains(x, y)))
    }

    /// The icon's square inside a cell, centered.
    pub fn icon_in(&self, cell: Rect) -> Rect {
        let size = self.metrics.icon.min(cell.w).min(cell.h);
        Rect::new(
            cell.x + (cell.w - size) / 2,
            cell.y + (cell.h - size) / 2,
            size,
            size,
        )
    }
}

/// A cell's box inside the strip, in anchored coordinates.
fn cell_in_strip(
    index: i32,
    cols: i32,
    rows: i32,
    orientation: Orientation,
    metrics: Metrics,
) -> Rect {
    let wrap_at = metrics.wrap_at.max(1);
    let (major, minor) = (index % wrap_at, index / wrap_at);
    let (col, row) = match orientation {
        Orientation::Horizontal => (major, minor),
        Orientation::Vertical => (minor, major),
    };
    // Clamped rather than asserted: a `wrap_at` change and an item arriving in the same frame
    // must not panic the tray, and the frame that follows corrects it anyway.
    let col = col.min(cols - 1).max(0);
    let row = row.min(rows - 1).max(0);
    let inset = BEVEL + PAD;
    Rect::new(
        inset + col * (metrics.cell + metrics.gap),
        inset + row * (metrics.cell + metrics.gap),
        metrics.cell,
        metrics.cell,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> Metrics {
        Metrics {
            icon: 22,
            cell: 28,
            gap: 2,
            margin: 8,
            wrap_at: 4,
        }
    }

    fn frame(count: usize, anchor: Anchor) -> Frame {
        Frame::new(
            count,
            anchor,
            Orientation::Horizontal,
            metrics(),
            None,
            0,
            Some((1000, 1000)),
        )
    }

    #[test]
    fn the_surface_is_exactly_the_strip_when_no_menu_is_open() {
        // Any slack here is a transparent margin, and a transparent margin on the bottom layer
        // eats clicks that belong to wlrix-desktop.
        let frame = frame(3, Anchor::BottomLeft);
        assert_eq!(frame.strip, Rect::new(0, 0, frame.width, frame.height));
        assert_eq!(frame.width, 3 * 28 + 2 * 2 + 2 * (BEVEL + PAD));
        assert_eq!(frame.height, 28 + 2 * (BEVEL + PAD));
    }

    #[test]
    fn a_full_run_wraps_to_a_second_row() {
        let frame = frame(5, Anchor::BottomLeft);
        assert_eq!(frame.width, 4 * 28 + 3 * 2 + 2 * (BEVEL + PAD));
        assert_eq!(frame.height, 2 * 28 + 2 + 2 * (BEVEL + PAD));
    }

    #[test]
    fn a_bottom_anchored_second_row_appears_above_the_first() {
        // Growing away from the anchored edge. Were it the other way round the strip would grow
        // off the bottom of the screen.
        let frame = frame(5, Anchor::BottomLeft);
        let first = frame.cell(0).unwrap();
        let wrapped = frame.cell(4).unwrap();
        assert!(
            wrapped.y < first.y,
            "{wrapped:?} should sit above {first:?}"
        );
        assert_eq!(wrapped.x, first.x, "and start a new run at the same column");
    }

    #[test]
    fn a_top_anchored_second_row_appears_below_the_first() {
        let frame = frame(5, Anchor::TopLeft);
        assert!(frame.cell(4).unwrap().y > frame.cell(0).unwrap().y);
    }

    #[test]
    fn a_right_anchored_strip_fills_leftward() {
        let frame = frame(3, Anchor::BottomRight);
        assert!(frame.cell(0).unwrap().x > frame.cell(2).unwrap().x);
        // ...and the first cell is still the one nearest the anchored corner.
        assert_eq!(frame.cell(0).unwrap().right(), frame.width - (BEVEL + PAD));
    }

    #[test]
    fn every_cell_is_inside_the_surface_for_every_corner() {
        for anchor in [
            Anchor::BottomLeft,
            Anchor::BottomRight,
            Anchor::TopLeft,
            Anchor::TopRight,
        ] {
            for orientation in [Orientation::Horizontal, Orientation::Vertical] {
                for count in 1..=9usize {
                    let frame = Frame::new(
                        count,
                        anchor,
                        orientation,
                        metrics(),
                        None,
                        0,
                        Some((1000, 1000)),
                    );
                    for index in 0..count {
                        let cell = frame.cell(index).unwrap();
                        assert!(
                            cell.x >= 0
                                && cell.y >= 0
                                && cell.right() <= frame.width
                                && cell.bottom() <= frame.height,
                            "{anchor:?} {orientation:?} {count} #{index}: {cell:?} \
                             escapes {}x{}",
                            frame.width,
                            frame.height
                        );
                    }
                    // And no two cells overlap, which a wrong wrap axis would cause.
                    for a in 0..count {
                        for b in (a + 1)..count {
                            let (one, two) = (frame.cell(a).unwrap(), frame.cell(b).unwrap());
                            assert_eq!(
                                one.intersect(two).w.min(one.intersect(two).h),
                                0,
                                "{anchor:?} {orientation:?}: cells {a} and {b} overlap"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_menu_grows_the_surface_inward_and_leaves_the_strip_at_its_corner() {
        let plain = frame(3, Anchor::BottomLeft);
        let posted = Frame::new(
            3,
            Anchor::BottomLeft,
            Orientation::Horizontal,
            metrics(),
            Some((186, 120)),
            0,
            Some((1000, 1000)),
        );
        assert_eq!(posted.height, plain.height + MENU_GAP + 120);
        // The anchored corner does not move, so the strip is still at the bottom.
        assert_eq!(posted.strip.bottom(), posted.height);
        // ...and the menu is above it.
        assert!(posted.popup.unwrap().bottom() <= posted.strip.y);
        // The cells traveled down with the strip rather than staying at the old y.
        assert_eq!(
            posted.cell(0).unwrap().y - posted.strip.y,
            plain.cell(0).unwrap().y - plain.strip.y
        );
    }

    #[test]
    fn a_menu_is_kept_on_the_screen() {
        // Posted from the last cell of a right-anchored tray, so the menu would otherwise run
        // off the far side.
        let narrow = Frame::new(
            3,
            Anchor::BottomLeft,
            Orientation::Horizontal,
            metrics(),
            Some((400, 60)),
            2,
            Some((300, 1000)),
        );
        let menu = narrow.popup.unwrap();
        assert!(menu.x >= 0, "{menu:?}");
    }

    #[test]
    fn cell_at_finds_what_cell_drew() {
        let frame = frame(5, Anchor::BottomLeft);
        for index in 0..5 {
            let cell = frame.cell(index).unwrap();
            assert_eq!(frame.cell_at(cell.x + 1, cell.y + 1), Some(index));
        }
        // The bevel of the well belongs to no cell.
        assert_eq!(frame.cell_at(0, 0), None);
    }

    #[test]
    fn an_empty_tray_still_has_a_drawable_size() {
        // A zero-sized wl_surface is a protocol error, so the well never collapses -- whether it
        // is *shown* when empty is `hide_when_empty`'s business, not this module's.
        let frame = frame(0, Anchor::BottomLeft);
        assert!(frame.width > 0 && frame.height > 0);
        assert_eq!(frame.cell(0), None);
    }
}
