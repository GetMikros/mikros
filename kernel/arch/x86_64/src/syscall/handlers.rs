//! The system call handlers, one per call in `kernel/syscalls.yaml`,
//! called by the generated dispatch after it has checked every handle
//! argument.

use super::{Checked, UserIn, UserOut, current_process};
use crate::memory_object::MemoryObject;
use crate::process::Process;
use crate::publish::{Outputs, Work};
use crate::sched::Thread;
use crate::thread_state::{self, DEBUG_SIZE, GENERAL_SIZE, General};
use alloc::sync::Arc;
use alloc::vec::Vec;
use kernel_core::exception::{self, DEBUG_EVENTS};
use kernel_core::log::{MESSAGE_MAX, RECORD_SIZE, Severity};
use kernel_core::object::budget::{Budget, INFO_BUDGET};
use kernel_core::object::channel::{BYTES_MAX, Endpoint, HANDLES_MAX, Message};
use kernel_core::object::handles::Handle;
use kernel_core::object::memory::{INFO_MEMORY, PAGE_SIZE};
use kernel_core::object::resource::{KIND_IOPORT, KIND_PRIORITY, KIND_ROOT, Request, Resource};
use kernel_core::object::{
    INFO_BASIC, NAME_MAX, Name, Object, ObjectType, basic_info, default_rights, downcast, rights,
};
use kernel_core::regions::MapOptions;
use kernel_core::signal::{self as sig, ACTION_SIZE, Action, Info, UNBLOCKABLE};
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
    let Some(h) = handle else {
        return Ok(());
    };
    let process = current_process()?;
    process.handles().lock().close(h.value)?;
    crate::wait::handle_closed(&process, h.value);
    Ok(())
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
    table.replace_then(handle.value, rights, |new| out.write(new))?;
    crate::wait::handle_closed(&process, handle.value);
    Ok(())
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
        INFO_MEMORY if kind == ObjectType::MemoryObject => {
            copy(&mut record, &object::<MemoryObject>(handle).info())
        }
        crate::process::INFO_PROCESS if kind == ObjectType::Process => {
            copy(&mut record, &object::<Process>(handle).info())
        }
        crate::sched::INFO_THREAD if kind == ObjectType::Thread => {
            copy(&mut record, &object::<Thread>(handle).info())
        }
        _ => return Err(Error::WrongType),
    };
    let record = &record[..size];
    let size = record.len() as u64;
    // Both addresses are checked before anything is written.
    if !out_len.fits(1) || (len >= size && !buf.fits(size)) {
        return Err(Error::InvalidArgs);
    }
    let process = current_process()?;
    crate::output::information(process.aspace().as_ref(), buf.0, len, record, out_len.0)
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
    if let Some(arg) = arg {
        table.close(arg.value)?;
        crate::wait::handle_closed(&caller, arg.value);
    }
    Ok(())
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

/// Ends `thread`, after a `THREAD_EXITING` report on its way back to
/// user mode when its process asks for thread events.
fn end_thread(thread: &Arc<Thread>) {
    match thread.process() {
        Some(process) if process.debug_events() => thread.request_exit(),
        _ => Process::kill_thread(thread),
    }
}

pub fn thread_exit() -> Result<(), Error> {
    end_thread(&current_thread()?);
    Ok(())
}

pub fn process_exit(code: i64) -> Result<(), Error> {
    current_process()?.terminate(code);
    Ok(())
}

pub fn task_kill(handle: Checked) -> Result<(), Error> {
    match handle.object.header().kind() {
        ObjectType::Process => object::<Process>(handle).terminate(-1),
        _ => end_thread(&object(handle)),
    }
    Ok(())
}

pub fn mem_create(size: u64, options: u32, out: UserOut<Handle>) -> Result<(), Error> {
    let mem = MemoryObject::new(current_process()?.budget(), size, options)?;
    give(mem, ObjectType::MemoryObject, out)
}

pub fn mem_read(mem: Checked, offset: u64, buf: UserOut<u8>, len: u64) -> Result<(), Error> {
    let mem = object::<MemoryObject>(mem);
    if mem.is_physical() {
        return Err(Error::NotSupported);
    }
    mem.check_range(offset, len)?;
    if !buf.fits(len) {
        return Err(Error::InvalidArgs);
    }
    let process = current_process()?;
    let budget = Some(process.budget());
    let aspace = process.aspace();
    let mut work = Work::new(crate::sched::preempt_point);
    let source = mem.hold_for_read(offset, len, budget, &mut work)?;
    let outputs = Outputs::prepare(&[(buf.0, len)], budget, &mut work, |page| {
        aspace.hold_for_write(page)
    });
    let outputs = match outputs {
        Ok(outputs) => outputs,
        Err(e) => {
            source.release(&mut work);
            return Err(e);
        }
    };
    // Bytes go through a kernel buffer, as `buf` may be backed by the
    // object itself.
    let mut chunk = [0u8; 256];
    let mut done = 0;
    while done < len {
        let at = offset + done;
        let n = ((PAGE_SIZE - at % PAGE_SIZE).min(len - done) as usize).min(chunk.len());
        source.read(at, &mut chunk[..n]);
        outputs.write(0, done, &chunk[..n], &mut work);
        done += n as u64;
    }
    outputs.release(&mut work);
    source.release(&mut work);
    Ok(())
}

pub fn mem_write(mem: Checked, offset: u64, buf: UserIn<u8>, len: u64) -> Result<(), Error> {
    let mem = object::<MemoryObject>(mem);
    if mem.is_physical() {
        return Err(Error::NotSupported);
    }
    mem.check_range(offset, len)?;
    if !buf.fits(len) {
        return Err(Error::InvalidArgs);
    }
    let mut done = 0;
    mem.write(offset, len, |bytes| {
        buf.at(done).read_bytes(bytes)?;
        done += bytes.len() as u64;
        Ok(())
    })
}

pub fn mem_set_size(mem: Checked, size: u64) -> Result<(), Error> {
    object::<MemoryObject>(mem).set_size(size)
}

pub fn vm_map(
    process: Checked,
    options: u32,
    addr: u64,
    mem: Checked,
    offset: u64,
    len: u64,
    out_addr: UserOut<u64>,
) -> Result<(), Error> {
    let options = MapOptions::parse(options, addr)?;
    if len == 0 || !offset.is_multiple_of(PAGE_SIZE) {
        return Err(Error::InvalidArgs);
    }
    let needed = options.rights();
    if mem.rights & needed != needed {
        return Err(Error::AccessDenied);
    }
    let mem = object::<MemoryObject>(mem);
    mem.check_range(offset, len)?;
    if !out_addr.fits(1) {
        return Err(Error::InvalidArgs);
    }
    // The range check bounds `len` by the object size, so this cannot
    // overflow.
    let size = len.next_multiple_of(PAGE_SIZE);
    let aspace = object::<Process>(process).aspace().clone();
    let start = aspace.add_region(options, addr, size, &mem, offset)?;
    out_addr.write(start).inspect_err(|_| {
        aspace.remove_regions(start, start + size);
    })
}

/// The end of the `len` bytes from `addr`, rounded up to whole pages, or
/// `InvalidArgs` when `len` is zero or `addr` is not page-aligned.
fn page_range_end(addr: u64, len: u64) -> Result<u64, Error> {
    if len == 0 || !addr.is_multiple_of(PAGE_SIZE) {
        return Err(Error::InvalidArgs);
    }
    Ok(addr.saturating_add(len.saturating_add(PAGE_SIZE - 1) / PAGE_SIZE * PAGE_SIZE))
}

pub fn vm_unmap(process: Checked, addr: u64, len: u64) -> Result<(), Error> {
    let end = page_range_end(addr, len)?;
    object::<Process>(process)
        .aspace()
        .remove_regions(addr, end);
    Ok(())
}

pub fn vm_set_inheritance(
    process: Checked,
    addr: u64,
    len: u64,
    inheritance: u32,
) -> Result<(), Error> {
    let end = page_range_end(addr, len)?;
    if inheritance > 2 {
        return Err(Error::InvalidArgs);
    }
    object::<Process>(process)
        .aspace()
        .set_inheritance(addr, end, inheritance)
}

pub fn channel_create(
    options: u32,
    out0: UserOut<Handle>,
    out1: UserOut<Handle>,
) -> Result<(), Error> {
    if options != 0 {
        return Err(Error::InvalidArgs);
    }
    if !out0.fits(1) || !out1.fits(1) {
        return Err(Error::InvalidArgs);
    }
    let process = current_process()?;
    let budget = Some(process.budget());
    let (a, b) = Endpoint::pair(budget)?;
    let rights = default_rights(ObjectType::Channel);
    let mut table = process.handles().lock();
    let entries = [
        kernel_core::object::handles::Entry { object: a, rights },
        kernel_core::object::handles::Entry { object: b, rights },
    ];
    table.insert_many_then::<2>(&entries, budget, |values| {
        crate::output::channel_pair(
            process.aspace().as_ref(),
            [out0.0, out1.0],
            [values[0], values[1]],
        )
    })
}

pub fn channel_write(
    channel: Checked,
    bytes: UserIn<u8>,
    len: u64,
    handles: UserIn<Handle>,
    count: u64,
) -> Result<(), Error> {
    if len > BYTES_MAX as u64 || count > HANDLES_MAX as u64 {
        return Err(Error::OutOfRange);
    }
    let (len, count) = (len as usize, count as usize);
    let mut data = Vec::new();
    data.try_reserve_exact(len).map_err(|_| Error::NoMemory)?;
    data.resize(len, 0);
    bytes.read_bytes(&mut data)?;
    let mut values = [0; HANDLES_MAX];
    let values = &mut values[..count];
    handles.read_into(values)?;
    for (i, &h) in values.iter().enumerate() {
        if h == channel.value || values[..i].contains(&h) {
            return Err(Error::InvalidArgs);
        }
    }
    let process = current_process()?;
    let mut table = process.handles().lock();
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(count)
        .map_err(|_| Error::NoMemory)?;
    for &h in values.iter() {
        entries.push(table.check(h, &[], rights::TRANSFER)?.clone());
    }
    let message = Message::new(data, entries, Some(process.budget()))?;
    // A message that was not queued drops only its own references, as
    // the handles are still in the table.
    object::<Endpoint>(channel)
        .write(message)
        .map_err(|(e, _)| e)?;
    let process = current_process()?;
    for &h in values.iter() {
        table.remove(h).expect("checked above");
        crate::wait::handle_closed(&process, h);
    }
    Ok(())
}

pub fn channel_read(
    channel: Checked,
    bytes: UserOut<u8>,
    bytes_cap: u64,
    handles: UserOut<Handle>,
    handles_cap: u64,
    out_len: UserOut<u64>,
    out_count: UserOut<u64>,
) -> Result<(), Error> {
    if !out_len.fits(1) || !out_count.fits(1) {
        return Err(Error::InvalidArgs);
    }
    let process = current_process()?;
    let budget = Some(process.budget());
    let aspace = process.aspace();
    let mut table = process.handles().lock();
    // A message spans fewer pages than a check is due after, so nothing
    // waits while the locks are held.
    let mut work = Work::new(|| debug_assert!(false, "a check while locked"));
    let taken = object::<Endpoint>(channel).take(|data, entries| {
        let (len, count) = (data.len() as u64, entries.len() as u64);
        let sizes =
            || crate::output::channel_sizes(aspace.as_ref(), [out_len.0, out_count.0], len, count);
        if len > bytes_cap || count > handles_cap {
            sizes()?;
            return Err(Error::BufferTooSmall);
        }
        if !bytes.fits(len) || !handles.fits(count) {
            return Err(Error::InvalidArgs);
        }
        let ranges = [
            (bytes.0, len),
            (handles.0, count * size_of::<Handle>() as u64),
            (out_len.0, size_of::<u64>() as u64),
            (out_count.0, size_of::<u64>() as u64),
        ];
        let prepare = |work: &mut Work<_>| {
            Outputs::prepare(&ranges, budget, work, |page| aspace.hold_for_write(page))
        };
        crate::output::accept(entries, &mut table, budget, &mut work, prepare, sizes)
            .map(|(outputs, values)| (outputs, values, entries.len()))
    });
    drop(table);
    let ((outputs, values, count), message) = taken?;
    let mut work = Work::new(crate::sched::preempt_point);
    crate::output::deliver(outputs, &message.bytes, &values[..count], &mut work);
    Ok(())
}

pub fn exception_channel_create(
    process: Checked,
    options: u32,
    out: UserOut<Handle>,
) -> Result<(), Error> {
    if options & !DEBUG_EVENTS != 0 || !out.fits(1) {
        return Err(Error::InvalidArgs);
    }
    let caller = current_process()?;
    let end = Process::create_exception_channel(&object(process), options, caller.budget())?;
    give(end, ObjectType::Channel, out)
}

pub fn thread_resume_from_exception(thread: Checked, action: u32) -> Result<(), Error> {
    let thread = object::<Thread>(thread);
    if thread.state() != crate::sched::EXCEPTION {
        return Err(Error::BadState);
    }
    match action {
        exception::RESUME => {
            crate::sched::resume_exception(&thread, crate::sched::RESUMED);
        }
        exception::TERMINATE => {
            if let Some(process) = thread.process() {
                process.terminate(exception::EXIT_CODE);
            }
        }
        exception::DELIVER_SIGNAL => {
            let event = thread.exception_kind();
            if event == exception::kind::THREAD_STARTING || event == exception::kind::THREAD_EXITING
            {
                return Err(Error::BadState);
            }
            crate::sched::resume_exception(&thread, crate::sched::DELIVER_SIGNAL);
        }
        _ => return Err(Error::InvalidArgs),
    }
    Ok(())
}

/// The kinds of thread state.
const STATE_GENERAL: u32 = 0;
const STATE_EXTENDED: u32 = 1;
const STATE_DEBUG: u32 = 2;

/// The size of the layout of state `kind`.
fn state_size(kind: u32) -> Result<u64, Error> {
    match kind {
        STATE_GENERAL => Ok(GENERAL_SIZE as u64),
        STATE_EXTENDED => Ok(8 + crate::context::extended_state_size() as u64),
        STATE_DEBUG => Ok(DEBUG_SIZE as u64),
        _ => Err(Error::InvalidArgs),
    }
}

pub fn thread_read_state(
    thread: Checked,
    kind: u32,
    buf: UserOut<u8>,
    len: u64,
) -> Result<(), Error> {
    let thread = object::<Thread>(thread);
    let size = state_size(kind)?;
    if len < size {
        return Err(Error::BufferTooSmall);
    }
    if !buf.fits(size) {
        return Err(Error::InvalidArgs);
    }
    if kind == STATE_DEBUG {
        let regs = thread.with_stopped_debug(|r| *r).ok_or(Error::BadState)?;
        return buf.write_bytes(&thread_state::debug_bytes(&regs));
    }
    // The state is copied out after the thread is let go, as the copy can
    // fault.
    let mut general = [0u8; GENERAL_SIZE];
    let mut extended = Vec::new();
    extended
        .try_reserve_exact(size as usize)
        .map_err(|_| Error::NoMemory)?;
    thread
        .with_stopped_state(|frame, bases, ext| match kind {
            STATE_GENERAL => general = General::from_frame(frame, bases[0], bases[1]).bytes(),
            _ => {
                extended.extend_from_slice(&(ext.bytes().len() as u64).to_le_bytes());
                extended.extend_from_slice(ext.bytes());
            }
        })
        .ok_or(Error::BadState)?;
    match kind {
        STATE_GENERAL => buf.write_bytes(&general),
        _ => buf.write_bytes(&extended),
    }
}

pub fn thread_write_state(
    thread: Checked,
    kind: u32,
    buf: UserIn<u8>,
    len: u64,
) -> Result<(), Error> {
    let thread = object::<Thread>(thread);
    let size = state_size(kind)?;
    if len < size {
        return Err(Error::BufferTooSmall);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size as usize)
        .map_err(|_| Error::NoMemory)?;
    bytes.resize(size as usize, 0);
    buf.read_bytes(&mut bytes)?;
    if kind == STATE_DEBUG {
        let state: &[u8; DEBUG_SIZE] = bytes[..].try_into().expect("the DEBUG size");
        let user = |a| crate::aspace::in_user_range(a, 1);
        let regs = thread_state::parse_debug(state, user).ok_or(Error::InvalidArgs)?;
        return thread
            .with_stopped_debug(|r| *r = regs)
            .ok_or(Error::BadState);
    }
    if kind == STATE_GENERAL {
        let state: &[u8; GENERAL_SIZE] = bytes[..].try_into().expect("the GENERAL size");
        let user = |a| crate::aspace::in_user_range(a, 1);
        let general = General::parse(state, user).ok_or(Error::InvalidArgs)?;
        return thread
            .with_stopped_state(|frame, bases, _| {
                general.apply(frame);
                *bases = [general.fs_base, general.gs_base];
            })
            .ok_or(Error::BadState);
    }
    let (count, area) = bytes.split_at(8);
    let count = u64::from_le_bytes(count.try_into().expect("8 bytes"));
    thread
        .with_stopped_state(|_, _, ext| {
            let mask = thread_state::mxcsr_mask(ext.bytes());
            if count != area.len() as u64
                || !thread_state::xsave_loadable(area, crate::context::xcr0(), mask)
            {
                return Err(Error::InvalidArgs);
            }
            ext.bytes_mut().copy_from_slice(area);
            Ok(())
        })
        .ok_or(Error::BadState)?
}

pub fn signal_set_action(signo: u32, action: UserIn<u8>, old: UserOut<u8>) -> Result<(), Error> {
    if !sig::valid(signo) {
        return Err(Error::InvalidArgs);
    }
    let new = if action.is_null() {
        None
    } else {
        if signo == sig::KILL || signo == sig::STOP {
            return Err(Error::InvalidArgs);
        }
        let mut bytes = [0u8; ACTION_SIZE];
        action.read_bytes(&mut bytes)?;
        Some(Action::parse(&bytes, |a| {
            crate::aspace::in_user_range(a, 1)
        })?)
    };
    if !old.is_null() && !old.fits(ACTION_SIZE as u64) {
        return Err(Error::InvalidArgs);
    }
    let process = current_process()?;
    let threads = process.live_threads();
    let mut state = process.signals().lock();
    let slot = signo as usize - 1;
    if !old.is_null() {
        old.write_bytes(&state.actions[slot].bytes())?;
    }
    if let Some(new) = new {
        state.actions[slot] = new;
        if new.ignores(signo) {
            crate::signal::discard(&mut state, &threads, sig::bit(signo));
        }
    }
    Ok(())
}

pub fn signal_set_mask(how: u32, mask: u64, old: UserOut<u64>) -> Result<(), Error> {
    if how > 2 {
        return Err(Error::InvalidArgs);
    }
    let thread = current_thread()?;
    let mut state = thread.signals().lock();
    if !old.is_null() {
        old.write(state.mask)?;
    }
    let mask = mask & !UNBLOCKABLE;
    state.mask = match how {
        0 => state.mask | mask,
        1 => state.mask & !mask,
        _ => mask,
    };
    Ok(())
}

/// The minimum size of an alternate signal stack.
const ALTSTACK_MIN: u64 = 16384;

pub fn signal_set_altstack(base: u64, size: u64, old: UserOut<u64>) -> Result<(), Error> {
    let remove = base == 0 && size == 0;
    if !remove && (size < ALTSTACK_MIN || !crate::aspace::in_user_range(base, size)) {
        return Err(Error::InvalidArgs);
    }
    let thread = current_thread()?;
    let mut state = thread.signals().lock();
    if state.on_altstack(crate::percpu::user_rsp()) {
        return Err(Error::BadState);
    }
    if !old.is_null() {
        let (b, s) = state.altstack;
        old.write_all(&[b, s])?;
    }
    state.altstack = (base, size);
    Ok(())
}

pub fn process_signal(process: Checked, signo: u32, value: u64) -> Result<(), Error> {
    if signo == 0 {
        return Ok(());
    }
    if signo > sig::SIG_MAX {
        return Err(Error::InvalidArgs);
    }
    let process = object::<Process>(process);
    let state = process.state();
    if state != crate::process::RUNNING && state != crate::process::STOPPED {
        return Err(Error::BadState);
    }
    let info = Info {
        signo,
        code: sig::CODE_PROCESS,
        sender_koid: current_process()?.header().koid(),
        value,
        ..Info::default()
    };
    crate::signal::send(&process, None, info)
}

pub fn thread_signal(thread: Checked, signo: u32, value: u64) -> Result<(), Error> {
    if !sig::valid(signo) {
        return Err(Error::InvalidArgs);
    }
    let thread = object::<Thread>(thread);
    let state = thread.state();
    let process = thread.process().cloned();
    let (Some(process), false) = (
        process,
        state == crate::sched::NEW || state == crate::sched::DEAD,
    ) else {
        return Err(Error::BadState);
    };
    let info = Info {
        signo,
        code: sig::CODE_THREAD,
        sender_koid: current_process()?.header().koid(),
        value,
        ..Info::default()
    };
    crate::signal::send(&process, Some(&thread), info)
}

pub fn signal_get_pending(out: UserOut<u64>) -> Result<(), Error> {
    let thread = current_thread()?;
    let process = current_process()?;
    out.write(crate::signal::pending_mask(&process, &thread))
}

pub fn signal_return(context: u64) -> Result<(), Error> {
    // The context replaces every register on the way back to user mode,
    // so the status this returns is never seen.
    current_thread()?.request_sigreturn(context.max(1));
    Ok(())
}

pub fn signal_wait(mask: u64, deadline: i64, out: UserOut<u8>) -> Result<(), Error> {
    if !out.fits(sig::INFO_SIZE as u64) {
        return Err(Error::InvalidArgs);
    }
    let info = crate::signal::wait(mask, deadline)?;
    out.write_bytes(&info.bytes())
}

pub fn log_write(_log: Checked, severity: u32, message: UserIn<u8>, len: u64) -> Result<(), Error> {
    let severity = Severity::from_u32(severity).ok_or(Error::InvalidArgs)?;
    if len > MESSAGE_MAX as u64 {
        return Err(Error::InvalidArgs);
    }
    let mut bytes = [0u8; MESSAGE_MAX];
    let bytes = &mut bytes[..len as usize];
    message.read_bytes(bytes)?;
    if core::str::from_utf8(bytes).is_err() {
        return Err(Error::InvalidArgs);
    }
    crate::log::write_user(current_process()?.header().koid(), severity, bytes);
    Ok(())
}

pub fn log_read(_log: Checked, after: u64, out: UserOut<u8>) -> Result<(), Error> {
    if !out.fits(RECORD_SIZE as u64) {
        return Err(Error::InvalidArgs);
    }
    let record = crate::log::next_after(after).ok_or(Error::ShouldWait)?;
    out.write_bytes(&record.bytes())
}

pub fn log_console_release(_log: Checked) -> Result<(), Error> {
    crate::console::release();
    Ok(())
}

pub fn clock_get_monotonic(out: UserOut<i64>) -> Result<(), Error> {
    out.write(crate::time::now())
}

pub fn thread_sleep(deadline: i64) -> Result<(), Error> {
    crate::signal::sleep(deadline)
}

pub fn resource_create(
    parent: Checked,
    kind: u32,
    base: u64,
    size: u64,
    options: u32,
    out: UserOut<Handle>,
) -> Result<(), Error> {
    if !out.fits(1) {
        return Err(Error::InvalidArgs);
    }
    let request = Request {
        kind,
        base,
        size,
        options,
    };
    let budget = current_process()?.budget().clone();
    let resource = Resource::create(
        &object(parent),
        request,
        &budget,
        true,
        crate::reserved::denied,
    )?;
    give(resource, ObjectType::Resource, out)
}

pub fn process_ioports_enable(
    process: Checked,
    resource: Checked,
    base: u64,
    size: u64,
) -> Result<(), Error> {
    let resource = object::<Resource>(resource);
    if resource.kind() != KIND_IOPORT {
        return Err(Error::WrongType);
    }
    if size == 0 {
        return Err(Error::InvalidArgs);
    }
    if !resource.covers(base, size) {
        return Err(Error::OutOfRange);
    }
    object::<Process>(process).enable_ioports(base, base + size)
}

#[cfg(feature = "fuzz")]
pub fn fuzz_cov_enable(mem: Checked) -> Result<(), Error> {
    let thread = crate::sched::current().ok_or(Error::BadState)?;
    crate::coverage::enable(&thread, object::<MemoryObject>(mem))
}

#[cfg(feature = "fuzz")]
pub fn fuzz_cov_disable() -> Result<(), Error> {
    let thread = crate::sched::current().ok_or(Error::BadState)?;
    crate::coverage::disable(&thread);
    Ok(())
}

pub fn object_wait_one(
    handle: Checked,
    signals: u32,
    deadline: i64,
    observed: UserOut<u32>,
) -> Result<(), Error> {
    if !observed.fits(1) {
        return Err(Error::InvalidArgs);
    }
    let process = current_process()?;
    let state = crate::wait::wait_one(&handle.object, &process, handle.value, signals, deadline);
    // The deadline passing also reports the state.
    let current = handle.object.header().signals();
    observed.write(state.unwrap_or(current))?;
    state.map(drop)
}

pub fn object_signal(handle: Checked, clear: u32, set: u32) -> Result<(), Error> {
    kernel_core::object::signal_user(&*handle.object, clear, set)
}

pub fn event_create(options: u32, out: UserOut<Handle>) -> Result<(), Error> {
    if options != 0 {
        return Err(Error::InvalidArgs);
    }
    if !out.fits(1) {
        return Err(Error::InvalidArgs);
    }
    let event = kernel_core::object::event::Event::new(current_process()?.budget())?;
    give(event, ObjectType::Event, out)
}

pub fn timer_create(options: u32, out: UserOut<Handle>) -> Result<(), Error> {
    if options != 0 || !out.fits(1) {
        return Err(Error::InvalidArgs);
    }
    let timer = crate::ktimer::Timer::new(current_process()?.budget())?;
    give(timer, ObjectType::Timer, out)
}

pub fn timer_set(timer: Checked, deadline: i64) -> Result<(), Error> {
    object::<crate::ktimer::Timer>(timer).set(deadline)
}

pub fn timer_cancel(timer: Checked) -> Result<(), Error> {
    object::<crate::ktimer::Timer>(timer).cancel();
    Ok(())
}

pub fn thread_set_priority(
    thread: Checked,
    resource: Option<Checked>,
    priority: u32,
) -> Result<(), Error> {
    if priority > kernel_core::sched::MAX_PRIORITY {
        return Err(Error::InvalidArgs);
    }
    if priority > kernel_core::sched::DEFAULT_PRIORITY {
        let allowed = resource.is_some_and(|r| {
            let r = object::<Resource>(r);
            match r.kind() {
                KIND_ROOT => true,
                KIND_PRIORITY => r.covers(u64::from(priority), 1),
                _ => false,
            }
        });
        if !allowed {
            return Err(Error::AccessDenied);
        }
    }
    crate::sched::set_priority(&object::<Thread>(thread), priority);
    Ok(())
}
