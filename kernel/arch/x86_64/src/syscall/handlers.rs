//! The system call handlers, one per call in `kernel/syscalls.yaml`,
//! called by the generated dispatch after it has checked every handle
//! argument.

use super::{Checked, UserOut, current_process};
use kernel_core::object::handles::Handle;
use kernel_core::object::{INFO_BASIC, basic_info};
use kernel_core::status::Error;

pub fn handle_close(handle: Option<Checked>) -> Result<(), Error> {
    match handle {
        Some(h) => current_process()?.handles().lock().close(h.value),
        None => Ok(()),
    }
}

pub fn handle_duplicate(handle: Checked, rights: u32, out: UserOut<Handle>) -> Result<(), Error> {
    let process = current_process()?;
    let mut table = process.handles().lock();
    table
        .duplicate_then(handle.value, rights, |new| out.write(new))
        .map(drop)
}

pub fn handle_replace(handle: Checked, rights: u32, out: UserOut<Handle>) -> Result<(), Error> {
    let process = current_process()?;
    let mut table = process.handles().lock();
    table
        .replace_then(handle.value, rights, |new| out.write(new))
        .map(drop)
}

pub fn object_get_info(
    handle: Checked,
    topic: u32,
    buf: UserOut<u8>,
    len: u64,
    out_len: UserOut<u64>,
) -> Result<(), Error> {
    let record = match topic {
        INFO_BASIC => basic_info(&*handle.object, handle.rights),
        _ => return Err(Error::WrongType),
    };
    let size = record.len() as u64;
    // Both addresses are checked before anything is written.
    if !out_len.fits(1) || (len >= size && !buf.fits(size)) {
        return Err(Error::InvalidArgs);
    }
    if len < size {
        out_len.write(size)?;
        return Err(Error::BufferTooSmall);
    }
    buf.write_bytes(&record)?;
    out_len.write(size)
}
