use crate::pixels::*;
use crate::rects::*;
use crate::text::*;
use alloc::vec;
use alloc::vec::Vec;

#[test]
fn bgrx_is_xrgb8888() {
    let c = Converter::new(BGRX, 0, 0, 0).unwrap();
    assert!(c.is_identity());
    assert_eq!(c.convert(0xFF12_3456), 0x0012_3456);
}

#[test]
fn rgbx_swaps_red_and_blue() {
    let c = Converter::new(RGBX, 0, 0, 0).unwrap();
    assert!(!c.is_identity());
    assert_eq!(c.convert(0x0012_3456), 0x0056_3412);
}

#[test]
fn masks_place_and_scale_channels() {
    // 10 bits per channel, blue lowest.
    let c = Converter::new(MASKS, 0x3FF0_0000, 0x000F_FC00, 0x0000_03FF).unwrap();
    assert_eq!(c.convert(0x00FF_0080), 0x3FC0_0000 | 0x0000_0200);
    // 5-6-5 in the low 16 bits.
    let c = Converter::new(MASKS, 0xF800, 0x07E0, 0x001F).unwrap();
    assert_eq!(c.convert(0x00FF_FFFF), 0xFFFF);
    assert_eq!(c.convert(0x0008_0408), 0x0821);
    // The masks of XRGB8888 itself.
    assert!(
        Converter::new(MASKS, 0xFF_0000, 0xFF00, 0xFF)
            .unwrap()
            .is_identity()
    );
}

#[test]
fn bad_formats_and_masks() {
    assert_eq!(Converter::new(0, 0, 0, 0), None);
    assert_eq!(Converter::new(4, 0, 0, 0), None);
    assert_eq!(Converter::new(MASKS, 0, 0xFF00, 0xFF), None);
    assert_eq!(Converter::new(MASKS, 0xF0F0_0000, 0xFF00, 0xFF), None);
    assert!(Converter::new(MASKS, 0xFFFF_FFFF, 0xFF00, 0xFF).is_some());
}

#[test]
fn rectangles() {
    let screen = Rect::new(0, 0, 640, 480);
    assert!(screen.contains(&Rect::new(10, 10, 630, 470)));
    assert!(!screen.contains(&Rect::new(10, 10, 631, 470)));
    assert!(!screen.contains(&Rect::new(-1, 0, 10, 10)));
    assert_eq!(
        screen.intersect(&Rect::new(600, -20, 100, 40)),
        Some(Rect::new(600, 0, 40, 20))
    );
    assert_eq!(screen.intersect(&Rect::new(640, 0, 10, 10)), None);
    assert_eq!(screen.intersect(&Rect::new(0, 0, 0, 10)), None);
}

#[test]
fn damage_limits_the_copy() {
    let src = Rect::new(0, 0, 100, 100);
    assert_eq!(copied(&src, &[]), vec![src]);
    assert_eq!(
        copied(
            &src,
            &[
                Rect::new(10, 10, 5, 5),
                Rect::new(90, 90, 20, 20),
                Rect::new(200, 0, 1, 1)
            ]
        ),
        vec![Rect::new(10, 10, 5, 5), Rect::new(90, 90, 10, 10)]
    );
}

#[test]
fn printable_ascii_has_its_own_glyphs_and_the_rest_that_of_a_question_mark() {
    use crate::font::PRINTABLE;
    assert_eq!(glyph(' '), &PRINTABLE[0]);
    assert_eq!(glyph('A'), &PRINTABLE[0x21]);
    assert_eq!(glyph('~'), &PRINTABLE[0x5E]);
    for c in ['é', '\u{7f}', '\n', '漢'] {
        assert_eq!(glyph(c), glyph('?'));
    }
    assert_ne!(glyph('A'), glyph('?'));
}

#[test]
fn a_row_of_text_sets_the_glyph_bits_in_each_cell() {
    let (fg, bg) = (0xFF, 0x11);
    let y = 6;
    let pixels: Vec<u32> = row("AB", y, 20, fg, bg).collect();
    assert_eq!(pixels.len(), 20);
    for (cell, c) in ['A', 'B'].into_iter().enumerate() {
        for x in 0..CELL_WIDTH {
            let set = glyph(c)[y] & (0x80 >> x) != 0;
            assert_eq!(pixels[cell * CELL_WIDTH + x], if set { fg } else { bg });
        }
    }
    // Past the text, the background.
    assert!(pixels[16..].iter().all(|&p| p == bg));
}

#[test]
fn a_cell_cut_by_the_width_is_background() {
    let (fg, bg) = (0xFF, 0x11);
    // '#' has pixels set in the row and in its left half.
    let y = 4;
    assert_ne!(glyph('#')[y] & 0xF0, 0);
    let pixels: Vec<u32> = row("##", y, 12, fg, bg).collect();
    assert_eq!(pixels.len(), 12);
    assert!(pixels[..8].contains(&fg));
    assert!(pixels[8..].iter().all(|&p| p == bg));
    // A row past the cell's height is background too.
    assert!(row("##", CELL_HEIGHT, 16, fg, bg).all(|p| p == bg));
}
