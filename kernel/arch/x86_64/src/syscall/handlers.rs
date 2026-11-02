//! The system call handlers, one per call in `kernel/syscalls.yaml`,
//! called by the generated dispatch after it has checked every handle
//! argument.

use super::{Checked, UserOut, current_process};
use alloc::sync::Arc;
use kernel_core::object::budget::{Budget, INFO_BUDGET};
use kernel_core::object::handles::Handle;
use kernel_core::object::{INFO_BASIC, Object, ObjectType, basic_info, default_rights, downcast};
use kernel_core::status::Error;

/// The object of a handle the dispatch has checked to be a `T`.
fn object<T: Object>(handle: Checked) -> Arc<T> {
    downcast(handle.object).expect("the dispatch checked the object type")
}

/// Adds a handle with the default rights for `object` to the calling
/// process's table, charged to its budget, and writes it to `out`.
fn give(object: Arc<dyn Object>, kind: ObjectType, out: UserOut<Handle>) -> Result<(), Error> {
    let process = current_process()?;
    let mut table = process.handles().lock();
    let budget = Some(process.budget());
    table
        .insert_then(object, default_rights(kind), budget, |h| out.write(h))
        .map(drop)
}

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
        .duplicate_then(handle.value, rights, Some(process.budget()), |new| {
            out.write(new)
        })
        .map(drop)
}

pub fn handle_replace(handle: Checked, rights: u32, out: UserOut<Handle>) -> Result<(), Error> {
    let process = current_process()?;
    let mut table = process.handles().lock();
    table
        .replace_then(handle.value, rights, |new| out.write(new))
        .map(drop)
}

/// The size of the largest information record.
const RECORD_MAX: usize = 0x40;

pub fn object_get_info(
    handle: Checked,
    topic: u32,
    buf: UserOut<u8>,
    len: u64,
    out_len: UserOut<u64>,
) -> Result<(), Error> {
    fn copy(record: &mut [u8; RECORD_MAX], from: &[u8]) -> usize {
        record[..from.len()].copy_from_slice(from);
        from.len()
    }
    let mut record = [0u8; RECORD_MAX];
    let kind = handle.object.header().kind();
    let size = match topic {
        INFO_BASIC => copy(&mut record, &basic_info(&*handle.object, handle.rights)),
        INFO_BUDGET if kind == ObjectType::Budget => {
            copy(&mut record, &object::<Budget>(handle).info())
        }
        _ => return Err(Error::WrongType),
    };
    let record = &record[..size];
    let size = record.len() as u64;
    // Both addresses are checked before anything is written.
    if !out_len.fits(1) || (len >= size && !buf.fits(size)) {
        return Err(Error::InvalidArgs);
    }
    if len < size {
        out_len.write(size)?;
        return Err(Error::BufferTooSmall);
    }
    buf.write_bytes(record)?;
    out_len.write(size)
}

pub fn budget_create(
    parent: Checked,
    kernel_limit: u64,
    page_limit: u64,
    out: UserOut<Handle>,
) -> Result<(), Error> {
    let parent = object::<Budget>(parent);
    let budget = Budget::child(
        &parent,
        kernel_limit,
        page_limit,
        Some(current_process()?.budget()),
    )?;
    give(budget, ObjectType::Budget, out)
}

pub fn budget_set_limits(budget: Checked, kernel_limit: u64, page_limit: u64) -> Result<(), Error> {
    object::<Budget>(budget).set_limits(kernel_limit, page_limit);
    Ok(())
}

pub fn budget_set_thresholds(
    budget: Checked,
    kernel_threshold: u64,
    page_threshold: u64,
) -> Result<(), Error> {
    object::<Budget>(budget).set_thresholds(kernel_threshold, page_threshold);
    Ok(())
}
