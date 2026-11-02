use super::*;

/// Builds a BMP file with a 40-byte header, or a 56-byte one when `masks`
/// are given.
fn bmp(
    width: i32,
    height: i32,
    bits: u16,
    palette: &[u32],
    masks: Option<[u32; 4]>,
    rows: &[&[u8]],
) -> Vec<u8> {
    let header_size: u32 = if masks.is_some() { 56 } else { 40 };
    let pixel_offset = 14 + header_size + 4 * palette.len() as u32;
    let mut out = Vec::new();
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&pixel_offset.to_le_bytes());
    out.extend_from_slice(&header_size.to_le_bytes());
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    let compression = if masks.is_some() {
        BI_BITFIELDS
    } else {
        BI_RGB
    };
    out.extend_from_slice(&compression.to_le_bytes());
    out.extend_from_slice(&[0; 12]);
    out.extend_from_slice(&(palette.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    if let Some(masks) = masks {
        for m in masks {
            out.extend_from_slice(&m.to_le_bytes());
        }
    }
    for c in palette {
        out.extend_from_slice(&c.to_le_bytes());
    }
    for row in rows {
        out.extend_from_slice(row);
    }
    out
}

#[test]
fn reads_bottom_up_24bit_images() {
    // Two rows of two pixels, each row padded to 8 bytes, stored bottom-up.
    let data = bmp(
        2,
        2,
        24,
        &[],
        None,
        &[
            &[0, 0, 255, 0, 255, 0, 0, 0],
            &[255, 0, 0, 255, 255, 255, 0, 0],
        ],
    );
    let image = decode(&data).unwrap();
    assert_eq!((image.width, image.height), (2, 2));
    assert_eq!(
        image.pixels,
        [0xff0000ff, 0xffffffff, 0xffff0000, 0xff00ff00]
    );
}

#[test]
fn reads_top_down_32bit_images() {
    let data = bmp(1, -2, 32, &[], None, &[&[1, 2, 3, 99], &[4, 5, 6, 99]]);
    assert_eq!(decode(&data).unwrap().pixels, [0xff030201, 0xff060504]);
}

#[test]
fn reads_palette_images() {
    let palette = [0x000000, 0xffffff];
    let data = bmp(3, 1, 1, &palette, None, &[&[0b1010_0000, 0, 0, 0]]);
    assert_eq!(
        decode(&data).unwrap().pixels,
        [0xffffffff, 0xff000000, 0xffffffff]
    );
    let data = bmp(2, 1, 4, &[0x112233, 0x445566], None, &[&[0x10, 0, 0, 0]]);
    assert_eq!(decode(&data).unwrap().pixels, [0xff445566, 0xff112233]);
}

#[test]
fn reads_bitfields_with_alpha() {
    let masks = [0x00ff_0000, 0x0000_ff00, 0x0000_00ff, 0xff00_0000];
    let data = bmp(1, 1, 32, &[], Some(masks), &[&[0x30, 0x20, 0x10, 0x80]]);
    assert_eq!(decode(&data).unwrap().pixels, [0x80102030]);
    let masks565 = [0xf800, 0x07e0, 0x001f, 0];
    let data = bmp(1, 1, 16, &[], Some(masks565), &[&[0x1f, 0xf8, 0, 0]]);
    assert_eq!(decode(&data).unwrap().pixels, [0xffff00ff]);
}

#[test]
fn rejects_bad_files() {
    assert_eq!(decode(b"PNG"), Err(Error::NotBmp));
    let data = bmp(2, 2, 24, &[], None, &[&[0; 8]]);
    assert_eq!(decode(&data), Err(Error::Truncated));
    let mut compressed = bmp(1, 1, 8, &[0], None, &[&[0; 4]]);
    compressed[30] = 1;
    assert_eq!(decode(&compressed), Err(Error::Unsupported));
    let data = bmp(0, 1, 24, &[], None, &[]);
    assert_eq!(decode(&data), Err(Error::Unsupported));
}

#[test]
fn scales_large_images_down_keeping_the_aspect_ratio() {
    let image = Image {
        width: 4,
        height: 2,
        pixels: vec![
            0xff000000, 0xffffffff, 0xff000000, 0xff000000, 0xff000000, 0xffffffff, 0xff000000,
            0xff000000,
        ],
    };
    let small = image.fit_within(2);
    assert_eq!((small.width, small.height), (2, 1));
    assert_eq!(small.pixels, [0xff808080, 0xff000000]);

    let tall = Image {
        width: 100,
        height: 200,
        pixels: vec![0xff123456; 20000],
    };
    let fitted = tall.fit_within(72);
    assert_eq!((fitted.width, fitted.height), (36, 72));
    assert!(fitted.pixels.iter().all(|&p| p == 0xff123456));
}

#[test]
fn keeps_images_that_fit() {
    let image = Image {
        width: 72,
        height: 10,
        pixels: vec![1; 720],
    };
    assert_eq!(image.clone().fit_within(72), image);
}
