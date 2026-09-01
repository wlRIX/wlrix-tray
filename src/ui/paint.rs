// SPDX-License-Identifier: GPL-3.0-or-later
//! Drawing the tray.
//!
//! Everything here takes geometry that has already been decided ([`crate::layout`],
//! [`crate::menu`]) and artwork that has already been decoded ([`crate::pixmap`]), and puts
//! pixels in a buffer. It holds no state, resolves nothing, and asks no questions of the bus --
//! which is what makes the parts that *do* testable on their own.
//!
//! # The surface starts transparent
//!
//! With a menu open the surface is the union of the strip and the menu, and the corner between
//! them belongs to neither. Clearing to a color would put a rectangle of face gray on the
//! desktop; clearing to transparent leaves the wallpaper showing through, which is what a menu
//! hanging off a dock looks like.
//!
//! # Menu rows have no icons, deliberately
//!
//! `com.canonical.dbusmenu` rows may carry an `icon-name`, and [`crate::sni::dbusmenu`] reads it.
//! Nothing here draws it: 4Dwm menus are text, a groove and a cascade arrow, and putting a column
//! of GTK icons down the left of one would make the tray's menu the only menu in wlRIX that does
//! not match the other two.

use wlrix_ui::canvas::{Canvas, Rect};
use wlrix_ui::color::Rgb;
use wlrix_ui::mask::Mask;
use wlrix_ui::motif;
use wlrix_ui::palette::Palette;
use wlrix_ui::text::{Face, Fonts, Run};

use crate::layout::{BEVEL as STRIP_BEVEL, Frame};
use crate::menu::{self, Posted, Row};
use crate::pixmap::Pixmap;
use crate::sni::dbusmenu::Toggle;
use crate::tooltip::Tooltip;

/// How a cell is being interacted with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellState {
    Idle,
    Hovered,
    Pressed,
}

/// One cell, with its artwork already chosen and scaled.
pub struct Cell<'a> {
    pub icon: Option<&'a Pixmap>,
    pub overlay: Option<&'a Pixmap>,
    pub state: CellState,
    /// Whether the item is asking to be noticed, which fills the cell rather than just tinting it.
    pub attention: bool,
}

/// Everything one frame needs.
pub struct Scene<'a> {
    pub palette: &'a Palette,
    pub fonts: &'a mut Fonts,
    pub frame: &'a Frame,
    pub cells: &'a [Cell<'a>],
    pub menu: Option<&'a Posted>,
    /// Shown only when no menu is: the two share the frame's popup slot.
    pub tooltip: Option<&'a Tooltip>,
}

/// Paint the whole surface.
pub fn tray(canvas: &mut Canvas, scene: &mut Scene) {
    canvas.clear_transparent();
    strip(canvas, scene);
    let Some(origin) = scene.frame.popup else {
        return;
    };
    match (scene.menu, scene.tooltip) {
        // A menu wins if both are somehow set: they share one slot in the frame, so drawing both
        // would draw the second over the first at the same place.
        (Some(posted), _) => self::menu(canvas, scene.palette, scene.fonts, posted, origin),
        (None, Some(tip)) => tooltip(canvas, scene.palette, scene.fonts, tip, origin),
        (None, None) => {}
    }
}

/// The hover tip: a small panel of text, in the tooltip colors the palette already carries.
fn tooltip(canvas: &mut Canvas, palette: &Palette, fonts: &mut Fonts, tip: &Tooltip, origin: Rect) {
    use crate::tooltip::{BEVEL as TIP_BEVEL, LABEL_PX as TIP_PX};

    motif::panel(
        canvas,
        Rect::new(origin.x, origin.y, tip.width, tip.height),
        palette.tooltip_background,
        motif::Bevel::raised(
            palette.face_top_shadow,
            palette.face_bottom_shadow,
            TIP_BEVEL,
        ),
    );

    let rows = tip.rows();
    // The title is bold and the description is not, which is the whole of the hierarchy a
    // two-line label needs.
    let lines = std::iter::once((Face::Bold, tip.title.as_str())).chain(
        tip.description
            .iter()
            .map(|text| (Face::Regular, text.as_str())),
    );
    for ((face, text), row) in lines.zip(rows) {
        let ascent = fonts.ascent(face, TIP_PX);
        let line = fonts.line_height(face, TIP_PX);
        fonts.draw(
            canvas,
            Run {
                face,
                px: TIP_PX,
                x: origin.x + row.x,
                baseline: origin.y + row.y + (row.h - line) / 2 + ascent,
                color: palette.tooltip_foreground,
            },
            text,
        );
    }
}

/// The well and its cells.
fn strip(canvas: &mut Canvas, scene: &mut Scene) {
    let palette = scene.palette;
    motif::panel(
        canvas,
        scene.frame.strip,
        palette.panel,
        motif::Bevel::raised(
            palette.panel_top_shadow,
            palette.panel_bottom_shadow,
            STRIP_BEVEL,
        ),
    );

    for (index, cell) in scene.cells.iter().enumerate() {
        let Some(rect) = scene.frame.cell(index) else {
            continue;
        };
        self::cell(canvas, palette, scene.frame, rect, cell);
    }
}

/// One item's cell: its state, its icon, and any overlay.
fn cell(canvas: &mut Canvas, palette: &Palette, frame: &Frame, rect: Rect, cell: &Cell) {
    // An item asking for attention fills its cell, so it reads at a glance from across the
    // screen -- which is the entire point of the status.
    if cell.attention {
        canvas.fill_rect(rect, palette.select_fill);
    }
    match cell.state {
        // Flat against the well until pointed at: a strip of permanently raised tiles reads as
        // eight buttons rather than one tray.
        CellState::Idle => {}
        CellState::Hovered => motif::panel(
            canvas,
            rect,
            palette.face,
            motif::Bevel::raised(
                palette.face_top_shadow,
                palette.face_bottom_shadow,
                menu::BEVEL,
            ),
        ),
        CellState::Pressed => motif::panel(
            canvas,
            rect,
            palette.armed,
            motif::Bevel::sunken(
                palette.face_top_shadow,
                palette.face_bottom_shadow,
                menu::BEVEL,
            ),
        ),
    }

    let icon = frame.icon_in(rect);
    match cell.icon {
        Some(pixmap) => pixmap.draw(canvas, icon.x, icon.y),
        // An item with no artwork the tray can decode still gets a cell: the application is
        // running and the user should be able to reach its menu. A hollow square is a clearer
        // way of saying "something is here but it published no icon" than an empty gap.
        None => placeholder(canvas, palette, icon),
    }

    // Bottom-right, at half size, as every implementation of this places it.
    if let Some(overlay) = cell.overlay {
        overlay.draw(
            canvas,
            icon.right() - overlay.width,
            icon.bottom() - overlay.height,
        );
    }
}

/// The stand-in for an item that published no usable icon.
fn placeholder(canvas: &mut Canvas, palette: &Palette, rect: Rect) {
    let size = rect.w.min(rect.h);
    if size <= 2 {
        return;
    }
    let mut mask = Mask::new(size);
    mask.outline(1, 1, size - 2, size - 2);
    mask.draw(canvas, rect, palette.icon_tint);
}

/// The cascade: one raised panel per column, with its rows.
///
/// `origin` is where [`crate::layout::Frame`] put the menu block; everything [`Posted`] knows is
/// in menu-local coordinates, so the offset is applied once, here.
fn menu(canvas: &mut Canvas, palette: &Palette, fonts: &mut Fonts, posted: &Posted, origin: Rect) {
    let shift = |rect: Rect| Rect::new(origin.x + rect.x, origin.y + rect.y, rect.w, rect.h);
    let ascent = fonts.ascent(Face::Bold, menu::LABEL_PX);
    let line = fonts.line_height(Face::Bold, menu::LABEL_PX);
    let hovered = posted.hovered();

    for (index, column) in posted.columns().iter().enumerate() {
        motif::panel(
            canvas,
            shift(column.panel),
            palette.face,
            motif::Bevel::raised(
                palette.face_top_shadow,
                palette.face_bottom_shadow,
                menu::BEVEL,
            ),
        );

        for (kind, rect) in &column.rows {
            let rect = shift(*rect);
            match kind {
                Row::Separator => motif::groove(
                    canvas,
                    rect,
                    palette.face_top_shadow,
                    palette.face_bottom_shadow,
                ),
                Row::Header => {
                    let label = posted.header();
                    let width = fonts.width(Face::Bold, menu::LABEL_PX, label);
                    fonts.draw(
                        canvas,
                        Run {
                            face: Face::Bold,
                            px: menu::LABEL_PX,
                            x: rect.x + (rect.w - width) / 2,
                            baseline: rect.y + (rect.h - line) / 2 + ascent,
                            color: palette.foreground,
                        },
                        label,
                    );
                    // Closed off with a groove, as the IRIX menu had.
                    motif::groove(
                        canvas,
                        Rect::new(rect.x, rect.y + rect.h - 2, rect.w, 2),
                        palette.face_top_shadow,
                        palette.face_bottom_shadow,
                    );
                }
                Row::Item(entry_index) => {
                    let Some(entry) = posted.entries(index).get(*entry_index) else {
                        continue;
                    };
                    let lit = hovered
                        == Some(menu::Hit {
                            column: index,
                            entry: *entry_index,
                        });
                    // The pointed-at row stands proud of the panel, as a button does. A row that
                    // opens a submenu stays lit while that submenu is open, or the cascade looks
                    // detached from what opened it.
                    let open = entry.submenu && index + 1 < posted.columns().len();
                    if lit || open {
                        motif::panel(
                            canvas,
                            rect,
                            palette.title_active,
                            motif::Bevel::raised(
                                palette.face_top_shadow,
                                palette.face_bottom_shadow,
                                menu::BEVEL,
                            ),
                        );
                    }

                    // Disabled rows take the bottom shadow, which is how Motif grays a label out.
                    let color = if entry.enabled {
                        palette.foreground
                    } else {
                        palette.face_bottom_shadow
                    };
                    fonts.draw(
                        canvas,
                        Run {
                            face: Face::Bold,
                            px: menu::LABEL_PX,
                            x: rect.x + menu::LABEL_INSET,
                            baseline: rect.y + (rect.h - line) / 2 + ascent,
                            color,
                        },
                        &entry.label,
                    );

                    indicator(canvas, palette, rect, entry.toggle);
                    if entry.submenu {
                        arrow(canvas, palette, rect, color);
                    }
                }
            }
        }
    }
}

/// A check or radio mark in the row's label inset.
fn indicator(canvas: &mut Canvas, palette: &Palette, row: Rect, toggle: Toggle) {
    let (state, radio) = match toggle {
        Toggle::None => return,
        Toggle::Check(state) => (state, false),
        Toggle::Radio(state) => (state, true),
    };
    let size = 8;
    let box_rect = Rect::new(
        row.x + (menu::LABEL_INSET - size) / 2,
        row.y + (row.h - size) / 2,
        size,
        size,
    );
    // Motif draws the well sunken and fills it when set. Indeterminate gets the well and no
    // fill, which is the specification's third state and is otherwise indistinguishable from off.
    motif::panel(
        canvas,
        box_rect,
        palette.indicator_background,
        motif::Bevel::sunken(palette.face_top_shadow, palette.face_bottom_shadow, 1),
    );
    if state == Some(true) {
        let color: Rgb = if radio {
            palette.radio_color
        } else {
            palette.check_color
        };
        canvas.fill_rect(box_rect.inset(2), color);
    }
}

/// The right-pointing triangle on a row that opens a submenu.
///
/// [`wlrix_ui::widget::arrow`] draws the scrollbar stepper's, which only points up or down --
/// a cascade arrow points along the axis the cascade opens.
fn arrow(canvas: &mut Canvas, _palette: &Palette, row: Rect, color: Rgb) {
    let size = 4;
    let x = row.right() - menu::ARROW_W / 2 - size / 2;
    let y = row.y + row.h / 2;
    for step in 0..=size {
        // Widest at the base and one pixel at the apex, so it reads as a triangle at this size
        // rather than as a smudge.
        let half = size - step;
        for dy in -half..=half {
            canvas.put(x + step, y + dy, color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::Measured;
    use super::*;
    use crate::layout::{Anchor, Metrics, Orientation};
    use wlrix_ui::palette::DEFAULT;

    fn fresh(buffer: &mut Vec<u8>, width: i32, height: i32) -> Canvas<'_> {
        *buffer = vec![0u8; (width * height * 4) as usize];
        Canvas::new(buffer, width, height)
    }

    fn frame(count: usize) -> Frame {
        Frame::new(
            count,
            Anchor::BottomLeft,
            Orientation::Horizontal,
            Metrics::default(),
            None,
            0,
            Some((1000, 1000)),
        )
    }

    fn scene<'a>(frame: &'a Frame, cells: &'a [Cell<'a>], fonts: &'a mut Fonts) -> Scene<'a> {
        Scene {
            palette: DEFAULT,
            fonts,
            frame,
            cells,
            menu: None,
            tooltip: None,
        }
    }

    #[test]
    fn the_surface_outside_the_strip_stays_transparent() {
        // With a menu open the corner between the strip and the menu is on neither. Clearing to
        // a color would put a gray rectangle on the desktop.
        let frame = Frame::new(
            1,
            Anchor::BottomLeft,
            Orientation::Horizontal,
            Metrics::default(),
            Some((100, 100)),
            0,
            Some((1000, 1000)),
        );
        let mut fonts = Fonts::load().expect("system fonts");
        let cells = [Cell {
            icon: None,
            overlay: None,
            state: CellState::Idle,
            attention: false,
        }];
        let mut buffer = Vec::new();
        let mut canvas = fresh(&mut buffer, frame.width, frame.height);
        let mut scene = scene(&frame, &cells, &mut fonts);
        tray(&mut canvas, &mut scene);

        // The far corner from the strip, which is anchored bottom-left.
        let corner = canvas.get(frame.width - 1, 0);
        assert_eq!(corner, Rgb(0), "{corner:?} should be transparent");
        // ...and the strip itself is not.
        assert_ne!(canvas.get(2, frame.height - 2), Rgb(0));
    }

    #[test]
    fn a_hovered_cell_is_raised_and_a_pressed_one_inverts() {
        let frame = frame(1);
        let mut fonts = Fonts::load().expect("system fonts");
        let cell = frame.cell(0).unwrap();

        let mut buffer = Vec::new();
        let mut canvas = fresh(&mut buffer, frame.width, frame.height);
        let cells = [Cell {
            icon: None,
            overlay: None,
            state: CellState::Hovered,
            attention: false,
        }];
        tray(&mut canvas, &mut scene(&frame, &cells, &mut fonts));
        let hovered_top = canvas.get(cell.x + cell.w / 2, cell.y);

        let mut buffer = Vec::new();
        let mut canvas = fresh(&mut buffer, frame.width, frame.height);
        let cells = [Cell {
            icon: None,
            overlay: None,
            state: CellState::Pressed,
            attention: false,
        }];
        tray(&mut canvas, &mut scene(&frame, &cells, &mut fonts));
        let pressed_top = canvas.get(cell.x + cell.w / 2, cell.y);

        assert_eq!(hovered_top, DEFAULT.face_top_shadow, "raised: light on top");
        assert_eq!(
            pressed_top, DEFAULT.face_bottom_shadow,
            "pressed: it inverts"
        );
    }

    #[test]
    fn an_item_with_no_icon_still_draws_something() {
        // Otherwise a running application with an unreadable icon leaves a gap in the strip, and
        // there is nothing to click to reach its menu.
        let frame = frame(1);
        let mut fonts = Fonts::load().expect("system fonts");
        let cells = [Cell {
            icon: None,
            overlay: None,
            state: CellState::Idle,
            attention: false,
        }];
        let mut buffer = Vec::new();
        let mut canvas = fresh(&mut buffer, frame.width, frame.height);
        tray(&mut canvas, &mut scene(&frame, &cells, &mut fonts));

        let icon = frame.icon_in(frame.cell(0).unwrap());
        assert_eq!(
            canvas.get(icon.x + 1, icon.y + 1),
            DEFAULT.icon_tint,
            "the placeholder outline"
        );
    }

    #[test]
    fn a_tooltip_paints_in_the_popup_slot_the_menu_would_have_used() {
        // The two share one rectangle in the frame, so the failure this guards against is a
        // tooltip drawn at the strip's origin -- on top of the icons it is describing.
        let mut fonts = Fonts::load().expect("system fonts");
        let tip = crate::tooltip::Tooltip::new(
            &crate::sni::item::ToolTip {
                icon_name: String::new(),
                title: "キーボード - 日本語".into(),
                description: String::new(),
            },
            16,
            &mut Measured { fonts: &mut fonts },
        )
        .expect("a title is enough for a tooltip");

        let frame = Frame::new(
            1,
            Anchor::BottomLeft,
            Orientation::Horizontal,
            Metrics::default(),
            Some(tip.size()),
            0,
            Some((1000, 1000)),
        );
        let cells = [Cell {
            icon: None,
            overlay: None,
            state: CellState::Hovered,
            attention: false,
        }];
        let mut buffer = Vec::new();
        let mut canvas = fresh(&mut buffer, frame.width, frame.height);
        tray(
            &mut canvas,
            &mut Scene {
                palette: DEFAULT,
                fonts: &mut fonts,
                frame: &frame,
                cells: &cells,
                menu: None,
                tooltip: Some(&tip),
            },
        );

        let popup = frame.popup.expect("the frame made room for it");
        assert!(
            popup.bottom() <= frame.strip.y,
            "the tip sits above the strip, not over it"
        );
        // The padding inside the bevel, which no glyph reaches -- the middle of the panel is a
        // line of text.
        assert_eq!(
            canvas.get(popup.right() - 2, popup.bottom() - 2),
            DEFAULT.tooltip_background,
            "and it is filled where the frame put it"
        );
    }

    #[test]
    fn an_item_asking_for_attention_fills_its_cell() {
        let frame = frame(1);
        let mut fonts = Fonts::load().expect("system fonts");
        let cells = [Cell {
            icon: None,
            overlay: None,
            state: CellState::Idle,
            attention: true,
        }];
        let mut buffer = Vec::new();
        let mut canvas = fresh(&mut buffer, frame.width, frame.height);
        tray(&mut canvas, &mut scene(&frame, &cells, &mut fonts));

        let cell = frame.cell(0).unwrap();
        assert_eq!(canvas.get(cell.x, cell.y), DEFAULT.select_fill);
    }
}
