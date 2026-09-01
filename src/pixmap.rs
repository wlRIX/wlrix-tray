// SPDX-License-Identifier: GPL-3.0-or-later
//! Raw icon pixels, however they arrived.
//!
//! Two sources feed one type. A themed `IconName` is a file on disk, decoded by
//! [`wlrix_ui::image`]; an `IconPixmap` is raw bytes straight off the bus. Downstream neither
//! the layout nor the painter should care which, so both become a [`Pixmap`] here.
//!
//! # The wire format, and the two things it gets wrong by default
//!
//! `IconPixmap` is `a(iiay)`: width, height, and the pixels. The StatusNotifierItem
//! specification inherits its format from X11 cursors, which means:
//!
//! - **ARGB32 in network byte order** -- big-endian, on machines that are not. Reading it as a
//!   native `u32` gives blue where the alpha should be, which looks like a working icon in
//!   entirely the wrong colors rather than like an obvious failure.
//! - **Not premultiplied.** [`wlrix_ui::canvas::Canvas::blend_premultiplied`] wants premultiplied
//!   pixels, and so does averaging: mixing straight alpha makes a transparent pixel's color
//!   contribute at full strength, which is the dark halo around a scaled-down icon.
//!
//! Both are handled once, in [`Pixmap::from_argb32_be`], and never again.
//!
//! # Why this is not in `wlrix-ui`
//!
//! `Image`'s pixel vector is private at the pinned rev and there is no raw constructor. Adding
//! one would be a `wlrix-ui` commit and a pin bump across every consumer -- a four-repo lockstep
//! for sixty lines that only the tray will ever run. If a second component ever grows a use for
//! raw ARGB, that is the moment to move this.

use wlrix_ui::canvas::Canvas;

/// A decoded icon: premultiplied native-endian ARGB, ready to composite.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Pixmap {
    pub width: i32,
    pub height: i32,
    pixels: Vec<u32>,
}

impl Pixmap {
    /// Whether there is anything to draw.
    pub fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0 || self.pixels.is_empty()
    }

    /// One pixel, or fully transparent off the edge. Matches [`wlrix_ui::image::Image::get`].
    pub fn get(&self, x: i32, y: i32) -> u32 {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return 0;
        }
        self.pixels[(y * self.width + x) as usize]
    }

    /// Take one `(width, height, bytes)` triple off the bus.
    ///
    /// `None` for anything that does not describe a picture: a non-positive dimension, a byte
    /// count that does not match, or a size big enough that some item's bug would have us
    /// allocate the machine's memory. An item may legitimately offer several pixmaps and it is
    /// normal for one of them to be junk, so this is a routine answer rather than an error.
    pub fn from_argb32_be(width: i32, height: i32, bytes: &[u8]) -> Option<Self> {
        // 1024 is four times the largest size any icon theme ships. Past that it is a bug on the
        // other end of the bus, and believing it costs 4MB a frame.
        if width <= 0 || height <= 0 || width > 1024 || height > 1024 {
            return None;
        }
        let count = (width as usize).checked_mul(height as usize)?;
        if bytes.len() != count * 4 {
            return None;
        }
        let (groups, _) = bytes.as_chunks::<4>();
        let pixels = groups
            .iter()
            .map(|argb| {
                // Big-endian on the wire: alpha first, whatever this machine's byte order is.
                let (a, r, g, b) = (argb[0], argb[1], argb[2], argb[3]);
                let premultiply = |channel: u8| (channel as u32 * a as u32 + 127) / 255;
                ((a as u32) << 24) | (premultiply(r) << 16) | (premultiply(g) << 8) | premultiply(b)
            })
            .collect();
        Some(Self {
            width,
            height,
            pixels,
        })
    }

    /// Copy a decoded theme icon into the same representation.
    ///
    /// [`wlrix_ui::image::Image`] is already premultiplied native-endian ARGB, so this is a
    /// transcription rather than a conversion -- it exists only so that the painter has one type
    /// to draw rather than two.
    pub fn from_image(image: &wlrix_ui::image::Image) -> Self {
        let mut pixels = Vec::with_capacity((image.width * image.height).max(0) as usize);
        for y in 0..image.height {
            for x in 0..image.width {
                pixels.push(image.get(x, y));
            }
        }
        Self {
            width: image.width,
            height: image.height,
            pixels,
        }
    }

    /// This icon at `size` square, or a clone when it is already that size.
    ///
    /// Area averaging in premultiplied space: every destination pixel is the mean of the source
    /// pixels it covers. That is the right filter for the case that actually happens -- a 48px
    /// application icon squeezed into a 22px cell -- and it degenerates to nearest-neighbor when
    /// scaling up, which is honest about there being no more detail to show.
    pub fn scaled(&self, size: i32) -> Self {
        if self.is_empty() || size <= 0 {
            return Self::default();
        }
        if self.width == size && self.height == size {
            return self.clone();
        }
        let mut pixels = Vec::with_capacity((size * size) as usize);
        for y in 0..size {
            // Half-open source spans, at least one pixel wide, so no destination pixel is empty
            // and none is counted twice.
            let (y0, y1) = span(y, size, self.height);
            for x in 0..size {
                let (x0, x1) = span(x, size, self.width);
                let (mut a, mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
                for sy in y0..y1 {
                    for sx in x0..x1 {
                        let pixel = self.get(sx, sy);
                        a += (pixel >> 24) & 0xff;
                        r += (pixel >> 16) & 0xff;
                        g += (pixel >> 8) & 0xff;
                        b += pixel & 0xff;
                        n += 1;
                    }
                }
                let n = n.max(1);
                pixels.push(((a / n) << 24) | ((r / n) << 16) | ((g / n) << 8) | (b / n));
            }
        }
        Self {
            width: size,
            height: size,
            pixels,
        }
    }

    /// Draw with the top-left at `(x, y)`, compositing over what is there.
    pub fn draw(&self, canvas: &mut Canvas, x: i32, y: i32) {
        for row in 0..self.height {
            for column in 0..self.width {
                let pixel = self.get(column, row);
                if pixel >> 24 == 0 {
                    continue;
                }
                canvas.blend_premultiplied(x + column, y + row, pixel);
            }
        }
    }
}

/// The half-open source span a destination pixel covers, never empty.
fn span(index: i32, out_of: i32, source: i32) -> (i32, i32) {
    let start = index * source / out_of;
    // The ceiling of `(index + 1) * source / out_of`, written out because `div_ceil` is stable
    // only for the unsigned integers. At least one pixel wide, so no destination pixel is empty.
    let span = (index + 1) * source;
    let end = ((span + out_of - 1) / out_of).max(start + 1);
    (start, end.min(source))
}

/// Pick the pixmap to use for a cell of `size`, out of everything the item offered.
///
/// Prefers the smallest one at least as large as the cell -- downscaling keeps detail, upscaling
/// invents it -- and falls back to the largest available when every offer is too small. Items
/// commonly publish one 22px bitmap and nothing else, so the fallback is the common path.
pub fn best(pixmaps: &[(i32, i32, Vec<u8>)], size: i32) -> Option<Pixmap> {
    let decoded: Vec<Pixmap> = pixmaps
        .iter()
        .filter_map(|(width, height, bytes)| Pixmap::from_argb32_be(*width, *height, bytes))
        .collect();
    let area = |pixmap: &Pixmap| pixmap.width as i64 * pixmap.height as i64;
    decoded
        .iter()
        .filter(|pixmap| pixmap.width >= size && pixmap.height >= size)
        .min_by_key(|pixmap| area(pixmap))
        .or_else(|| decoded.iter().max_by_key(|pixmap| area(pixmap)))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One opaque red pixel, as it arrives on the bus.
    fn red() -> Vec<u8> {
        vec![0xff, 0xff, 0x00, 0x00]
    }

    #[test]
    fn the_wire_format_is_big_endian_argb() {
        let pixmap = Pixmap::from_argb32_be(1, 1, &red()).unwrap();
        // Read as a native u32 on a little-endian machine this would be 0x0000ffff -- a
        // transparent cyan, which draws as nothing at all.
        assert_eq!(pixmap.get(0, 0), 0xffff_0000);
    }

    #[test]
    fn alpha_is_premultiplied_on_the_way_in() {
        // Half-transparent white: 0x80 alpha with full color channels.
        let pixmap = Pixmap::from_argb32_be(1, 1, &[0x80, 0xff, 0xff, 0xff]).unwrap();
        let pixel = pixmap.get(0, 0);
        assert_eq!(pixel >> 24, 0x80);
        // Every color channel scaled down to match, or `blend_premultiplied` draws it too
        // bright and every antialiased edge on the bus gets a white fringe.
        for shift in [16, 8, 0] {
            assert_eq!((pixel >> shift) & 0xff, 0x80, "channel at {shift}");
        }
    }

    #[test]
    fn junk_is_declined_rather_than_believed() {
        assert!(Pixmap::from_argb32_be(0, 4, &[]).is_none());
        assert!(Pixmap::from_argb32_be(-1, 4, &[]).is_none());
        // A byte count that does not match the dimensions.
        assert!(Pixmap::from_argb32_be(2, 2, &red()).is_none());
        // A size nothing legitimate would ask for.
        assert!(Pixmap::from_argb32_be(4096, 4096, &[]).is_none());
    }

    #[test]
    fn scaling_down_averages_rather_than_dropping_pixels() {
        // A 2x2 with one opaque red pixel and three transparent ones, into 1x1. Nearest
        // neighbor would answer either "red" or "nothing"; the average is a quarter-covered red.
        let mut bytes = red();
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        let small = Pixmap::from_argb32_be(2, 2, &bytes).unwrap().scaled(1);
        assert_eq!(small.width, 1);
        let pixel = small.get(0, 0);
        assert_eq!(pixel >> 24, 0x3f, "a quarter of full alpha");
        // Premultiplied, so the red channel came down with the alpha -- straight-alpha averaging
        // would leave it at 0xff and the icon would come out a bright halo.
        assert_eq!((pixel >> 16) & 0xff, 0x3f);
    }

    #[test]
    fn every_destination_pixel_gets_at_least_one_source_pixel() {
        // The classic off-by-one here leaves a blank last row or column.
        for source in [1, 3, 22, 48, 64] {
            for size in [1, 16, 22, 64, 128] {
                let bytes = red().repeat((source * source) as usize);
                let pixmap = Pixmap::from_argb32_be(source, source, &bytes)
                    .unwrap()
                    .scaled(size);
                assert_eq!((pixmap.width, pixmap.height), (size, size));
                for y in 0..size {
                    for x in 0..size {
                        assert_eq!(pixmap.get(x, y) >> 24, 0xff, "{source}->{size} at {x},{y}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_best_pixmap_is_the_smallest_one_big_enough() {
        let make = |size: i32| (size, size, red().repeat((size * size) as usize));
        let offers = vec![make(16), make(32), make(64)];
        assert_eq!(best(&offers, 22).unwrap().width, 32);
        // Nothing big enough: take the largest rather than blowing a 16px bitmap up to 128.
        assert_eq!(best(&offers, 128).unwrap().width, 64);
        // An item that offered only junk gets no icon, not a panic.
        assert!(best(&[(0, 0, vec![])], 22).is_none());
        assert!(best(&[], 22).is_none());
    }
}
