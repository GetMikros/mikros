//! Decompressing zstd extents.

use super::Error;
use super::file::COMPRESSED_MAX;
use alloc::vec::Vec;

pub(super) const COMPRESS_ZSTD: u8 = 3;

/// Decompresses one zstd frame from `data`, which may be followed by
/// padding, to the `size` bytes it holds.
pub(super) fn decompress(compression: u8, data: &[u8], size: u64) -> Result<Vec<u8>, Error> {
    use ruzstd::decoding::{BlockDecodingStrategy, FrameDecoder};
    if compression != COMPRESS_ZSTD {
        return Err(Error::Compression(compression));
    }
    if size > COMPRESSED_MAX {
        return Err(Error::Corrupt("a compressed extent is too large"));
    }
    let corrupt = |_| Error::Corrupt("a zstd frame is not valid");
    let mut decoder = FrameDecoder::new();
    let mut source = data;
    decoder.init(&mut source).map_err(corrupt)?;
    let mut out = Vec::with_capacity(size as usize);
    loop {
        decoder
            .decode_blocks(&mut source, BlockDecodingStrategy::UptoBlocks(1))
            .map_err(corrupt)?;
        if let Some(bytes) = decoder.collect() {
            out.extend_from_slice(&bytes);
        }
        if out.len() as u64 > size {
            return Err(Error::Corrupt("a zstd frame holds too much"));
        }
        if decoder.is_finished() {
            break;
        }
    }
    if let Some(bytes) = decoder.collect() {
        out.extend_from_slice(&bytes);
    }
    if out.len() as u64 > size {
        return Err(Error::Corrupt("a zstd frame holds too much"));
    }
    out.resize(size as usize, 0);
    Ok(out)
}
