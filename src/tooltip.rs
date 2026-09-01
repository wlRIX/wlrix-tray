// SPDX-License-Identifier: GPL-3.0-or-later
//! The hover tooltip: what an item's `ToolTip` property says, laid out.
//!
//! Small, but not decoration. An input-method indicator draws one keyboard whatever method is
//! active -- fcitx5 publishes `IconName = "input-keyboard-symbolic"` for both Mozc and plain
//! Japanese keyboard input -- so the icon cannot say which is on and the tooltip is the only
//! thing that can. That is the wnn-era use case exactly.
//!
//! Geometry only, like [`crate::menu`]: measured through a closure so this module stays free of
//! font machinery and can be tested without loading one. It shares the frame's popup slot with a
//! posted menu, because the two are never on screen together -- once the menu is open the user
//! has stopped asking what the cell is.

pub use wlrix_ui::canvas::Rect;

use crate::sni::item::ToolTip;

/// Text size, matching a menu row's.
pub const LABEL_PX: f32 = 14.0;
/// Bevel thickness of the panel.
pub const BEVEL: i32 = 1;
/// Space between the panel edge and the text.
pub const PAD: i32 = 4;
/// Space between the title line and the description.
pub const LINE_GAP: i32 = 2;
/// The widest a tooltip gets before its description wraps.
const MAX_WIDTH: i32 = 320;
/// How many lines of description are shown. Past this it is cut with an ellipsis: a tooltip is a
/// label, and an item that puts a paragraph in one does not get to own the screen.
///
/// Public because the caller does the wrapping, through [`Text::wrap`], and has to cap it here.
pub const MAX_LINES: usize = 4;

/// What a tooltip needs from a font.
///
/// One trait rather than two closures, because the caller's two closures would both borrow the
/// same `Fonts` -- it measures mutably, caching its shaping as it goes -- and two `&mut` borrows
/// of it cannot be alive at once. A single implementor holds the one borrow.
pub trait Text {
    /// The width of a run of the title face.
    fn width(&mut self, text: &str) -> i32;
    /// `text` broken to `width`, in the description face.
    fn wrap(&mut self, text: &str, width: i32) -> Vec<String>;
}

/// A laid-out tooltip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tooltip {
    /// The bold first line: the item's title.
    pub title: String,
    /// The description, already wrapped.
    pub description: Vec<String>,
    /// Panel size, bevel included.
    pub width: i32,
    pub height: i32,
    /// Row height, the same for every line.
    line: i32,
}

impl Tooltip {
    /// Lay out what an item published, or `None` when it published nothing worth showing.
    ///
    /// Measured through [`Text`] rather than a font, for the same reason [`crate::menu`] takes a
    /// closure: this module stays free of font machinery and its geometry can be tested without
    /// loading one.
    pub fn new(tip: &ToolTip, line: i32, text: &mut impl Text) -> Option<Self> {
        if tip.is_empty() {
            return None;
        }
        // A description identical to the title is what several items publish; showing it twice
        // makes the tooltip look like a bug rather than like more information.
        let description = if tip.description == tip.title {
            String::new()
        } else {
            tip.description.clone()
        };

        // The title sets the width, up to the cap; the description wraps into whatever that gives
        // it. Sizing to the description instead would let one long sentence stretch the panel past
        // the title it belongs to.
        let title_width = text.width(&tip.title);
        let text_width = title_width.clamp(0, MAX_WIDTH);
        let description = if description.is_empty() {
            Vec::new()
        } else {
            text.wrap(&description, MAX_WIDTH.max(text_width))
        };

        // A plain loop rather than an iterator chain: `text` is borrowed mutably, so a closure
        // capturing it cannot coexist with anything else that uses it.
        let mut widest = title_width;
        for line in &description {
            widest = widest.max(text.width(line));
        }
        let widest = widest.min(MAX_WIDTH);

        let lines = 1 + description.len() as i32;
        let gaps = if description.is_empty() { 0 } else { LINE_GAP };
        Some(Self {
            title: tip.title.clone(),
            description,
            width: widest + 2 * (BEVEL + PAD),
            height: lines * line + gaps + 2 * (BEVEL + PAD),
            line,
        })
    }

    /// The size the frame has to make room for.
    pub fn size(&self) -> (i32, i32) {
        (self.width, self.height)
    }

    /// Where each line's box is, relative to the panel's top-left, title first.
    pub fn rows(&self) -> Vec<Rect> {
        let inner = self.width - 2 * (BEVEL + PAD);
        let mut rows = vec![Rect::new(BEVEL + PAD, BEVEL + PAD, inner, self.line)];
        let mut top = BEVEL + PAD + self.line + LINE_GAP;
        for _ in &self.description {
            rows.push(Rect::new(BEVEL + PAD, top, inner, self.line));
            top += self.line;
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for the font: a fixed width per character, and wrapping at spaces.
    struct Fixed;

    impl Text for Fixed {
        fn width(&mut self, text: &str) -> i32 {
            text.chars().count() as i32 * 8
        }

        fn wrap(&mut self, text: &str, width: i32) -> Vec<String> {
            let limit = (width / 8).max(1) as usize;
            let mut lines = Vec::new();
            let mut line = String::new();
            for word in text.split_whitespace() {
                if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > limit {
                    lines.push(std::mem::take(&mut line));
                }
                if !line.is_empty() {
                    line.push(' ');
                }
                line.push_str(word);
            }
            if !line.is_empty() {
                lines.push(line);
            }
            lines.truncate(MAX_LINES);
            lines
        }
    }

    fn measure(text: &str) -> i32 {
        Fixed.width(text)
    }

    fn tip(title: &str, description: &str) -> ToolTip {
        ToolTip {
            icon_name: String::new(),
            title: title.to_owned(),
            description: description.to_owned(),
        }
    }

    fn laid_out(title: &str, description: &str) -> Option<Tooltip> {
        Tooltip::new(&tip(title, description), 16, &mut Fixed)
    }

    #[test]
    fn an_item_with_nothing_to_say_gets_no_tooltip() {
        // Most items publish an empty ToolTip. Showing an empty beveled box for them would put
        // a gray smudge on the desktop every time the pointer crossed the strip.
        assert!(laid_out("", "").is_none());
    }

    #[test]
    fn a_title_alone_is_one_line() {
        // fcitx5's is exactly this: "キーボード - 日本語" with an empty description.
        let tooltip = laid_out("キーボード - 日本語", "").expect("a title is enough");
        assert!(tooltip.description.is_empty());
        assert_eq!(tooltip.rows().len(), 1);
        assert_eq!(tooltip.height, 16 + 2 * (BEVEL + PAD));
        assert_eq!(
            tooltip.width,
            measure("キーボード - 日本語") + 2 * (BEVEL + PAD)
        );
    }

    #[test]
    fn a_description_that_repeats_the_title_is_dropped() {
        // Several items publish the same string twice, and showing it twice reads as a bug.
        let tooltip = laid_out("Steam", "Steam").expect("the title is still shown");
        assert!(tooltip.description.is_empty());
        assert_eq!(tooltip.rows().len(), 1);
    }

    #[test]
    fn a_description_wraps_and_adds_rows() {
        let tooltip = laid_out(
            "Mozc",
            "Japanese input method with a rather long description here",
        )
        .expect("something to show");
        assert!(tooltip.description.len() > 1, "{:?}", tooltip.description);
        assert_eq!(tooltip.rows().len(), 1 + tooltip.description.len());
        // Every row is inside the panel, and they do not overlap.
        for pair in tooltip.rows().windows(2) {
            assert!(pair[0].bottom() <= pair[1].y, "{pair:?}");
        }
        let last = *tooltip.rows().last().unwrap();
        assert!(last.bottom() + BEVEL + PAD <= tooltip.height);
    }

    #[test]
    fn one_enormous_line_does_not_stretch_the_panel_across_the_screen() {
        let tooltip = laid_out("Item", &"word ".repeat(200)).expect("something to show");
        assert!(tooltip.width <= MAX_WIDTH + 2 * (BEVEL + PAD));
        assert!(tooltip.description.len() <= MAX_LINES);
    }
}
