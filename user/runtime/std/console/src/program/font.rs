//! FreeType: the monospaced font at a pixel size, and its glyphs as
//! 8-bit coverage bitmaps.

use std::ffi::{CString, c_char, c_int, c_long, c_uint, c_ulong, c_ushort, c_void};

type Library = *mut c_void;
type Pos = c_long;

#[repr(C)]
struct Generic {
    data: *mut c_void,
    finalizer: *mut c_void,
}

#[repr(C)]
struct Vector {
    x: Pos,
    y: Pos,
}

#[repr(C)]
struct Bitmap {
    rows: c_uint,
    width: c_uint,
    pitch: c_int,
    buffer: *mut u8,
    num_grays: c_ushort,
    pixel_mode: u8,
    palette_mode: u8,
    palette: *mut c_void,
}

/// The leading fields of `FT_GlyphSlotRec`, up to the bitmap's position.
#[repr(C)]
struct GlyphSlot {
    library: Library,
    face: *mut c_void,
    next: *mut c_void,
    glyph_index: c_uint,
    generic: Generic,
    metrics: [Pos; 8],
    linear_hori_advance: c_long,
    linear_vert_advance: c_long,
    advance: Vector,
    format: c_uint,
    bitmap: Bitmap,
    bitmap_left: c_int,
    bitmap_top: c_int,
}

#[repr(C)]
struct SizeMetrics {
    x_ppem: c_ushort,
    y_ppem: c_ushort,
    x_scale: c_long,
    y_scale: c_long,
    ascender: Pos,
    descender: Pos,
    height: Pos,
    max_advance: Pos,
}

/// The leading fields of `FT_SizeRec`.
#[repr(C)]
struct Size {
    face: *mut c_void,
    generic: Generic,
    metrics: SizeMetrics,
}

/// The leading fields of `FT_FaceRec`, up to its size.
#[repr(C)]
struct Face {
    num_faces: c_long,
    face_index: c_long,
    face_flags: c_long,
    style_flags: c_long,
    num_glyphs: c_long,
    family_name: *mut c_char,
    style_name: *mut c_char,
    num_fixed_sizes: c_int,
    available_sizes: *mut c_void,
    num_charmaps: c_int,
    charmaps: *mut c_void,
    generic: Generic,
    bbox: [Pos; 4],
    units_per_em: c_ushort,
    shorts: [i16; 7],
    glyph: *mut GlyphSlot,
    size: *mut Size,
}

const FT_LOAD_RENDER: i32 = 1 << 2;
/// FT_LOAD_TARGET_LIGHT: light hinting, which keeps glyph widths.
const FT_LOAD_TARGET_LIGHT: i32 = 1 << 16;

#[link(name = "freetype")]
unsafe extern "C" {
    fn FT_Init_FreeType(library: *mut Library) -> c_int;
    fn FT_Done_FreeType(library: Library) -> c_int;
    fn FT_New_Face(
        library: Library,
        path: *const c_char,
        index: c_long,
        face: *mut *mut Face,
    ) -> c_int;
    fn FT_Done_Face(face: *mut Face) -> c_int;
    fn FT_Set_Pixel_Sizes(face: *mut Face, width: c_uint, height: c_uint) -> c_int;
    fn FT_Load_Char(face: *mut Face, code: c_ulong, flags: i32) -> c_int;
}

/// A rendered glyph: its coverage, `width` by `rows` bytes, and where
/// its top left lies from the pen on the baseline.
pub struct Glyph {
    pub width: usize,
    pub rows: usize,
    pub left: i32,
    pub top: i32,
    pub coverage: Vec<u8>,
}

/// A font face at a pixel size.
pub struct Font {
    library: Library,
    face: *mut Face,
    /// The cell's width and height, and the baseline's distance from
    /// the cell's top, in pixels.
    pub cell: (usize, usize),
    pub baseline: usize,
}

/// 26.6 fixed point to pixels, rounded up.
fn pixels(v: Pos) -> usize {
    usize::try_from((v + 63) >> 6).unwrap_or(0)
}

impl Font {
    /// The face in the file at `path`, `size` pixels high.
    pub fn open(path: &str, size: u32) -> Option<Font> {
        let path = CString::new(path).ok()?;
        let mut library = std::ptr::null_mut();
        // SAFETY: the out pointer is valid; results are checked.
        unsafe {
            if FT_Init_FreeType(&mut library) != 0 {
                return None;
            }
            let mut face = std::ptr::null_mut();
            if FT_New_Face(library, path.as_ptr(), 0, &mut face) != 0 {
                FT_Done_FreeType(library);
                return None;
            }
            let mut font = Font {
                library,
                face,
                cell: (0, 0),
                baseline: 0,
            };
            font.resize(size)?;
            Some(font)
        }
    }

    /// Sets the size to `size` pixels high.
    pub fn resize(&mut self, size: u32) -> Option<()> {
        // SAFETY: the face is valid while `self` lives, and FreeType
        // keeps its size record.
        unsafe {
            if FT_Set_Pixel_Sizes(self.face, 0, size) != 0 {
                return None;
            }
            let metrics = &(*(*self.face).size).metrics;
            let ascender = pixels(metrics.ascender);
            let descender = pixels(-metrics.descender);
            let width = pixels(metrics.max_advance).max(1);
            self.cell = (width, (ascender + descender).max(1));
            self.baseline = ascender;
        }
        Some(())
    }

    /// The glyph of `c`, rendered, or None when the face cannot render
    /// it.
    pub fn glyph(&self, c: char) -> Option<Glyph> {
        // SAFETY: as for `resize`; the bitmap stays valid until the next
        // load, and is copied out before it.
        unsafe {
            if FT_Load_Char(
                self.face,
                c as c_ulong,
                FT_LOAD_RENDER | FT_LOAD_TARGET_LIGHT,
            ) != 0
            {
                return None;
            }
            let slot = &*(*self.face).glyph;
            let bitmap = &slot.bitmap;
            let (width, rows) = (bitmap.width as usize, bitmap.rows as usize);
            let pitch = bitmap.pitch.unsigned_abs() as usize;
            let mut coverage = vec![0u8; width * rows];
            if !bitmap.buffer.is_null() {
                for row in 0..rows {
                    let from = bitmap.buffer.add(row * pitch);
                    std::ptr::copy_nonoverlapping(
                        from,
                        coverage[row * width..].as_mut_ptr(),
                        width,
                    );
                }
            }
            Some(Glyph {
                width,
                rows,
                left: slot.bitmap_left,
                top: slot.bitmap_top,
                coverage,
            })
        }
    }
}

impl Drop for Font {
    fn drop(&mut self) {
        // SAFETY: both were made by `open` and are dropped once.
        unsafe {
            FT_Done_Face(self.face);
            FT_Done_FreeType(self.library);
        }
    }
}
