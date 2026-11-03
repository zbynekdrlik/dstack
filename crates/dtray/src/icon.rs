// SPDX-License-Identifier: MIT OR Apache-2.0

//! The tray icon's pixels: four squares (the PC's devices) in the view's
//! colour on a clear ground, made in code, so dtray ships no image file.

use crate::model::Colour;

/// The icon's width and height, pixels.
pub const SIZE: u32 = 32;
/// The clear edge around the squares and the gap between them, pixels.
pub const GAP: u32 = 2;
/// A square's side, pixels: two squares and three gaps fill [`SIZE`].
pub const SQUARE: u32 = (SIZE - 3 * GAP) / 2;
const _: () = assert!(2 * SQUARE + 3 * GAP == SIZE);

/// The colour's RGB.
pub fn rgb(c: Colour) -> [u8; 3] {
    match c {
        Colour::Grey => [128, 128, 128],
        Colour::Green => [46, 160, 67],
        Colour::Orange => [235, 140, 0],
        Colour::Red => [215, 40, 40],
    }
}

/// Whether the column or row `at` is inside a square.
fn inside(at: u32) -> bool {
    let first = GAP..GAP + SQUARE;
    let second = 2 * GAP + SQUARE..2 * (GAP + SQUARE);
    first.contains(&at) || second.contains(&at)
}

/// The icon in `c`: RGBA, row by row, [`SIZE`] × [`SIZE`].
pub fn rgba(c: Colour) -> Vec<u8> {
    let [r, g, b] = rgb(c);
    let mut out = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            if inside(x) && inside(y) {
                out.extend_from_slice(&[r, g, b, 255]);
            } else {
                out.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(img: &[u8], x: u32, y: u32) -> [u8; 4] {
        let i = ((y * SIZE + x) * 4) as usize;
        [img[i], img[i + 1], img[i + 2], img[i + 3]]
    }

    #[test]
    fn four_squares_on_a_clear_ground() {
        let img = rgba(Colour::Green);
        assert_eq!(img.len(), (SIZE * SIZE * 4) as usize);
        let on = [46, 160, 67, 255];
        let off = [0, 0, 0, 0];
        // Each square's edges, and the clear edge and gap around them.
        for (at, want) in [
            (0, off),
            (1, off),
            (2, on),
            (14, on),
            (15, off),
            (16, off),
            (17, on),
            (29, on),
            (30, off),
            (31, off),
        ] {
            assert_eq!(pixel(&img, at, at), want, "diagonal at {at}");
            assert_eq!(pixel(&img, at, 2), want, "row 2 at {at}");
            assert_eq!(pixel(&img, 17, at), want, "column 17 at {at}");
        }
        assert_eq!(pixel(&img, 5, 15), off, "the gap between rows");
        assert_eq!(SQUARE, 13);
    }

    #[test]
    fn each_colour_is_its_own() {
        let all = [Colour::Grey, Colour::Green, Colour::Orange, Colour::Red];
        assert_eq!(rgb(Colour::Grey), [128, 128, 128]);
        assert_eq!(rgb(Colour::Orange), [235, 140, 0]);
        assert_eq!(rgb(Colour::Red), [215, 40, 40]);
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(rgb(*a), rgb(*b));
            }
            let [r, g, b] = rgb(*a);
            assert_eq!(pixel(&rgba(*a), 2, 2), [r, g, b, 255]);
        }
    }
}
