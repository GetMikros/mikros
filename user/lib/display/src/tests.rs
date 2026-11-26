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

#[test]
fn a_commit_copies_the_whole_buffer_when_the_scanout_changes() {
    let whole = Rect::new(0, 0, 100, 50);
    let src = Rect::new(0, 0, 80, 50);
    let damage = [Rect::new(10, 10, 5, 5)];
    // A new buffer, rectangle or mode: all of the buffer, flushed whole.
    let p = presented(&whole, &src, &damage, true);
    assert_eq!((p.copies, p.flush), (vec![whole], whole));
    // The same one with no damage: all of the buffer too.
    let p = presented(&whole, &src, &[], false);
    assert_eq!((p.copies, p.flush), (vec![whole], whole));
    // The same one with damage: the damage within the source, flushed as
    // the box around it.
    let two = [Rect::new(10, 10, 5, 5), Rect::new(40, 20, 10, 10)];
    let p = presented(&whole, &src, &two, false);
    assert_eq!(p.copies, two.to_vec());
    assert_eq!(p.flush, Rect::new(10, 10, 40, 20));
    // Damage outside the source copies nothing, and flushes the source.
    let p = presented(&whole, &src, &[Rect::new(90, 0, 5, 5)], false);
    assert_eq!((p.copies, p.flush), (vec![], src));
}

#[test]
fn two_buffers_that_alternate_copy_what_changed_since_each_was_last_copied() {
    let whole = Rect::new(0, 0, 100, 100);
    let mut lacking = Lacking::default();
    let a = Rect::new(10, 10, 5, 5);
    let b = Rect::new(60, 60, 5, 5);
    let c = Rect::new(30, 0, 5, 5);
    // The first presentation of each buffer copies all of it.
    let p = lacking.present(1, &whole, &whole, &[a], false);
    assert_eq!(p.copies, vec![whole]);
    let p = lacking.present(2, &whole, &whole, &[b], true);
    assert_eq!(p.copies, vec![whole]);
    // Buffer 1 again: the damage of this commit and of the commit that
    // presented buffer 2, since buffer 1 was last copied.
    let p = lacking.present(1, &whole, &whole, &[c], true);
    assert_eq!(p.copies, vec![b, c]);
    assert_eq!(p.flush, Rect::new(30, 0, 35, 65));
    // And buffer 2: this commit's damage and that of the last.
    let p = lacking.present(2, &whole, &whole, &[a], true);
    assert_eq!(p.copies, vec![c, a]);
    // A commit with no damage names the whole plane, which a buffer that
    // alternates copies whole the next time too.
    let p = lacking.present(1, &whole, &whole, &[], true);
    assert_eq!(p.copies, vec![whole]);
    let p = lacking.present(2, &whole, &whole, &[a], true);
    assert_eq!(p.copies, vec![whole]);
}

#[test]
fn a_buffer_whose_copy_failed_or_that_was_released_is_copied_whole_again() {
    let whole = Rect::new(0, 0, 100, 100);
    let a = Rect::new(10, 10, 5, 5);
    let mut lacking = Lacking::default();
    lacking.present(1, &whole, &whole, &[a], false);
    lacking.present(2, &whole, &whole, &[a], true);
    lacking.forget(1);
    assert_eq!(
        lacking.present(1, &whole, &whole, &[a], true).copies,
        vec![whole]
    );
    // Without earlier content on the plane all is copied and the others
    // are forgotten.
    let p = lacking.present(2, &whole, &whole, &[a], false);
    assert_eq!(p.copies, vec![whole]);
    assert_eq!(
        lacking.present(1, &whole, &whole, &[a], true).copies,
        vec![whole]
    );
    lacking.reset();
    assert_eq!(
        lacking.present(2, &whole, &whole, &[a], true).copies,
        vec![whole]
    );
}

#[test]
fn what_a_buffer_lacks_is_kept_as_all_of_it_when_it_grows_long() {
    let whole = Rect::new(0, 0, 100, 100);
    let mut lacking = Lacking::default();
    lacking.present(1, &whole, &whole, &[Rect::new(0, 0, 1, 1)], false);
    lacking.present(2, &whole, &whole, &[Rect::new(0, 0, 1, 1)], true);
    // Buffer 1 is not shown again for a long time.
    for i in 0..70 {
        let r = Rect::new(i, 0, 1, 1);
        lacking.present(2, &whole, &whole, &[r], true);
    }
    let p = lacking.present(1, &whole, &whole, &[Rect::new(0, 50, 1, 1)], true);
    assert_eq!(p.copies, vec![whole]);
}
