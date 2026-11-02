//! The system call handlers, one per call in `kernel/syscalls.yaml`,
//! called by the generated dispatch after it has checked every handle
//! argument.

use super::{Checked, UserIn, UserOut, current_process};
use crate::process::Process;
use crate::sched::Thread;
use alloc::sync::Arc;
use kernel_core::object::budget::{Budget, INFO_BUDGET};
use kernel_core::object::handles::Handle;
use kernel_core::object::{
    INFO_BASIC, NAME_MAX, Name, Object, ObjectType, basic_info, default_rights, downcast, rights,
};
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

/// Reads an object name of `len` bytes from `name`.
fn read_name(name: UserIn<u8>, len: u64) -> Result<Name, Error> {
    let len = usize::try_from(len)
        .ok()
        .filter(|&l| l <= NAME_MAX)
        .ok_or(Error::InvalidArgs)?;
    let mut bytes = [0u8; NAME_MAX];
    name.read_bytes(&mut bytes[..len])?;
    Name::new(&bytes[..len])
}

pub fn process_create(
    budget: Checked,
    name: UserIn<u8>,
    name_len: u64,
    out: UserOut<Handle>,
) -> Result<(), Error> {
    let name = read_name(name, name_len)?;
    let process = Process::new(&object(budget), name, true)?;
    give(process, ObjectType::Process, out)
}

pub fn thread_create(
    process: Checked,
    name: UserIn<u8>,
    name_len: u64,
    out: UserOut<Handle>,
) -> Result<(), Error> {
    let name = read_name(name, name_len)?;
    let thread = Process::create_thread(&object(process), name, true)?;
    give(thread, ObjectType::Thread, out)
}

pub fn process_start(
    process: Checked,
    thread: Checked,
    entry: u64,
    stack: u64,
    arg: Option<Checked>,
    arg2: u64,
) -> Result<(), Error> {
    let caller = current_process()?;
    // The caller's table stays locked until `arg` is closed in it, so the
    // handle cannot change in between.
    let mut table = caller.handles().lock();
    let moved = match &arg {
        Some(arg) => Some(table.check(arg.value, &[], rights::TRANSFER)?.clone()),
        None => None,
    };
    let (process, thread) = (object::<Process>(process), object::<Thread>(thread));
    let budget = Some(caller.budget());
    Process::start(&process, &thread, entry, stack, moved, arg2, budget)?;
    match arg {
        Some(arg) => table.close(arg.value),
        None => Ok(()),
    }
}

pub fn thread_start(
    thread: Checked,
    entry: u64,
    stack: u64,
    arg1: u64,
    arg2: u64,
) -> Result<(), Error> {
    Process::start_thread(&object(thread), entry, stack, arg1, arg2)
}

pub fn thread_set_user_base(which: u32, value: u64) -> Result<(), Error> {
    let base = match which {
        0 => crate::sched::UserBase::Fs,
        1 => crate::sched::UserBase::Gs,
        _ => return Err(Error::InvalidArgs),
    };
    if value != 0 && !crate::aspace::in_user_range(value, 1) {
        return Err(Error::InvalidArgs);
    }
    crate::sched::set_user_base(base, value);
    Ok(())
}

/// The calling thread. The return to user mode ends it once it is DEAD.
fn current_thread() -> Result<Arc<Thread>, Error> {
    crate::sched::current().ok_or(Error::BadHandle)
}

pub fn thread_exit() -> Result<(), Error> {
    Process::kill_thread(&current_thread()?);
    Ok(())
}

pub fn process_exit(code: i64) -> Result<(), Error> {
    current_process()?.terminate(code);
    Ok(())
}

pub fn task_kill(handle: Checked) -> Result<(), Error> {
    match handle.object.header().kind() {
        ObjectType::Process => object::<Process>(handle).terminate(-1),
        _ => Process::kill_thread(&object(handle)),
    }
    Ok(())
}
