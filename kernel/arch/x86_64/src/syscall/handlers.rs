//! The system call handlers, one per call in `kernel/syscalls.yaml`,
//! called by the generated dispatch after it has checked every handle
//! argument.

use super::{Checked, UserIn, UserOut, bytes_of, current_process};
use crate::memory_object::{Held, MemoryObject};
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
use kernel_core::object::stream::{QUEUE_BYTES as STREAM_QUEUE_BYTES, StreamEnd};
use kernel_core::object::{
    INFO_BASIC, NAME_MAX, Name, Object, ObjectType, basic_info, default_rights, downcast, rights,
};
use kernel_core::regions::MapOptions;
use kernel_core::signal::{self as sig, ACTION_SIZE, Action, Info, UNBLOCKABLE};
use kernel_core::status::Error;

/// Prepares the output `ranges` of `process`, which have been checked to
/// fit, by obtaining writable backing for every page, waiting for page
/// requests as needed. No lock may be held.
fn prepare(process: &Arc<Process>, ranges: &[(u64, u64)]) -> Result<Outputs<Held>, Error> {
    let aspace = process.aspace();
    let mut work = Work::new(crate::sched::preempt_point);
    Outputs::prepare(ranges, Some(process.budget()), &mut work, |page| {
        aspace.hold_for_write(page)
    })
}

/// Writes each of `values` from the start of the prepared output with
/// its index, then releases the outputs. Cannot fail.
fn publish(outputs: Outputs<Held>, values: &[&[u8]]) {
    let mut work = Work::new(crate::sched::preempt_point);
    for (output, bytes) in values.iter().enumerate() {
        outputs.write(output, 0, bytes, &mut work);
    }
    outputs.release(&mut work);
}

/// The object of a handle the dispatch has checked to be a `T`.
fn object<T: Object>(handle: Checked) -> Arc<T> {
    downcast(handle.object).expect("the dispatch checked the object type")
}

/// Adds a handle with `rights` for `object` to the calling process's
/// table, charged to its budget, and writes it to `out`, which is
/// prepared first.
fn give_with(object: Arc<dyn Object>, rights: u32, out: UserOut<Handle>) -> Result<(), Error> {
    let process = current_process()?;
    let outputs = prepare(&process, &[out.range(1)])?;
    let h = process
        .handles()
        .lock()
        .insert(object, rights, Some(process.budget()))?;
    publish(outputs, &[bytes_of(&[h])]);
    Ok(())
}

/// Like `give_with`, with the default rights for `kind`.
fn give(object: Arc<dyn Object>, kind: ObjectType, out: UserOut<Handle>) -> Result<(), Error> {
    give_with(object, default_rights(kind), out)
}

/// Adds handles for both `objects` with `rights` to the calling process's
/// table, charged to its budget, keeping neither unless both fit, and
/// writes them to `outs`, which are prepared first.
fn give_pair(
    objects: [Arc<dyn Object>; 2],
    rights: u32,
    outs: [UserOut<Handle>; 2],
) -> Result<(), Error> {
    let process = current_process()?;
    let outputs = prepare(&process, &[outs[0].range(1), outs[1].range(1)])?;
    let entries = objects.map(|object| kernel_core::object::handles::Entry { object, rights });
    let mut values = [0; 2];
    process.handles().lock().insert_many_then::<2>(
        &entries,
        Some(process.budget()),
        |handles| {
            values.copy_from_slice(handles);
            Ok(())
        },
    )?;
    publish(outputs, &[bytes_of(&values[..1]), bytes_of(&values[1..])]);
    Ok(())
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
    let outputs = prepare(&process, &[out.range(1)])?;
    let new = process
        .handles()
        .lock()
        .duplicate(handle.value, rights, Some(process.budget()))?;
    publish(outputs, &[bytes_of(&[new])]);
    Ok(())
}

pub fn handle_replace(handle: Checked, rights: u32, out: UserOut<Handle>) -> Result<(), Error> {
    let process = current_process()?;
    let outputs = prepare(&process, &[out.range(1)])?;
    let new = process.handles().lock().replace(handle.value, rights)?;
    crate::wait::handle_closed(&process, handle.value);
    publish(outputs, &[bytes_of(&[new])]);
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
    // A short buffer gets only the size.
    if len < size {
        let outputs = prepare(&process, &[out_len.range(1)])?;
        publish(outputs, &[bytes_of(&[size])]);
        return Err(Error::BufferTooSmall);
    }
    let outputs = prepare(&process, &[buf.range(size), out_len.range(1)])?;
    publish(outputs, &[record, bytes_of(&[size])]);
    Ok(())
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
    let handle_rights = mem.rights;
    let mem = object::<MemoryObject>(mem);
    mem.check_range(offset, len)?;
    mem.check_revoked()?;
    if options.perms & kernel_core::regions::PERM_WRITE != 0 {
        mem.check_writable()?;
    }
    if !out_addr.fits(1) {
        return Err(Error::InvalidArgs);
    }
    // The range check bounds `len` by the object size, so this cannot
    // overflow.
    let size = len.next_multiple_of(PAGE_SIZE);
    let outputs = prepare(&current_process()?, &[out_addr.range(1)])?;
    let aspace = object::<Process>(process).aspace().clone();
    let max = MapOptions::allowed_by(handle_rights);
    let start = aspace.add_region(options, addr, size, &mem, offset, max)?;
    publish(outputs, &[bytes_of(&[start])]);
    Ok(())
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
    let (a, b) = Endpoint::pair(Some(process.budget()))?;
    give_pair([a, b], default_rights(ObjectType::Channel), [out0, out1])
}

pub fn channel_write(
    channel: Checked,
    bytes: UserIn<u8>,
    len: u64,
    handles: UserIn<Handle>,
    count: u64,
) -> Result<(), Error> {
    let value = channel.value;
    let endpoint = object::<Endpoint>(channel);
    send(&endpoint, value, bytes, len, handles, count, None)
}

/// Writes the message of `len` bytes from `bytes` and `count` handles
/// from `handles` to `endpoint`, reached through `value`, and moves the
/// handles out of the caller's table once it is written. With `id`, the
/// message's transaction ID is replaced by it first. Wakes the caller of
/// a call that the message replies to.
fn send(
    endpoint: &Endpoint,
    value: Handle,
    bytes: UserIn<u8>,
    len: u64,
    handles: UserIn<Handle>,
    count: u64,
    id: Option<u32>,
) -> Result<(), Error> {
    if len > BYTES_MAX as u64 || count > HANDLES_MAX as u64 {
        return Err(Error::OutOfRange);
    }
    let (len, count) = (len as usize, count as usize);
    let mut data = Vec::new();
    data.try_reserve_exact(len).map_err(|_| Error::NoMemory)?;
    data.resize(len, 0);
    bytes.read_bytes(&mut data)?;
    if let Some(id) = id {
        data[..4].copy_from_slice(&id.to_le_bytes());
    }
    let mut values = [0; HANDLES_MAX];
    let values = &mut values[..count];
    handles.read_into(values)?;
    for (i, &h) in values.iter().enumerate() {
        if h == value || values[..i].contains(&h) {
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
    let delivered = endpoint.write(message).map_err(|(e, _)| e)?;
    for &h in values.iter() {
        table.remove(h).expect("checked above");
        crate::wait::handle_closed(&process, h);
    }
    if let Some((koid, id)) = delivered {
        crate::wait::reply_delivered(koid, id);
    }
    Ok(())
}

/// The size of the `channel_call` argument structure.
const CALL_ARGS_SIZE: usize = 0x40;

pub fn channel_call(
    channel: Checked,
    options: u32,
    deadline: i64,
    call_args: UserIn<u8>,
    out: UserOut<u8>,
) -> Result<(), Error> {
    if options != 0 || !out.fits(16) {
        return Err(Error::InvalidArgs);
    }
    let mut raw = [0u8; CALL_ARGS_SIZE];
    call_args.read_bytes(&mut raw)?;
    let field = |at: usize| u64::from_le_bytes(raw[at..at + 8].try_into().expect("8 bytes"));
    let (wr_len, wr_count) = (field(0x08), field(0x18));
    let (rd_cap, rd_handles_cap) = (field(0x28), field(0x38));
    if wr_len < 4 {
        return Err(Error::InvalidArgs);
    }
    let value = channel.value;
    let endpoint: Arc<Endpoint> = object(channel);
    let id = endpoint.begin_call()?;
    let request = send(
        &endpoint,
        value,
        UserIn::new(field(0x00)),
        wr_len,
        UserIn::new(field(0x10)),
        wr_count,
        Some(id),
    );
    let process = current_process()?;
    let reply = request.and_then(|()| crate::wait::wait_call(&endpoint, &process, id, deadline));
    let reply = match reply {
        Ok(reply) => reply,
        Err(e) => {
            // A reply that came meanwhile is dropped with no lock held.
            drop(endpoint.abandon_call(id));
            return Err(e);
        }
    };
    let (rd_bytes, rd_handles) = (
        UserOut::<u8>::new(field(0x20)),
        UserOut::<Handle>::new(field(0x30)),
    );
    let sizes = [UserOut::new(out.0), UserOut::new(out.0 + 8)];
    let (len, count) = (reply.bytes.len() as u64, reply.handles.len() as u64);
    // The reply and its handles go when it is dropped on failure.
    if len > rd_cap || count > rd_handles_cap {
        publish_sizes(&process, &sizes, len, count)?;
        return Err(Error::BufferTooSmall);
    }
    if !rd_bytes.fits(len) || !rd_handles.fits(count) {
        return Err(Error::InvalidArgs);
    }
    let ranges = message_ranges(&rd_bytes, &rd_handles, &sizes, len, count);
    let outputs = prepare(&process, &ranges)?;
    let installed = crate::output::install(
        &reply.handles,
        &mut process.handles().lock(),
        Some(process.budget()),
    );
    let mut work = Work::new(crate::sched::preempt_point);
    match installed {
        Ok(values) => {
            let handles = &values[..count as usize];
            crate::output::deliver(outputs, &reply.bytes, handles, &mut work);
            Ok(())
        }
        Err(e) => {
            end_undelivered(outputs, e, len, count, &mut work);
            Err(e)
        }
    }
}

/// Ends a delivery that failed with `e` after its outputs were prepared:
/// writes only the two sizes when the table could not hold the handles,
/// and releases the outputs.
fn end_undelivered<F: FnMut()>(
    outputs: Outputs<Held>,
    e: Error,
    len: u64,
    count: u64,
    work: &mut Work<F>,
) {
    if e == Error::NoResources {
        crate::output::deliver_sizes(outputs, len, count, work);
    } else {
        outputs.release(work);
    }
}

/// The output ranges of a received message of `len` bytes and `count`
/// handles: its bytes, its handles and the two sizes.
fn message_ranges(
    bytes: &UserOut<u8>,
    handles: &UserOut<Handle>,
    sizes: &[UserOut<u64>; 2],
    len: u64,
    count: u64,
) -> [(u64, u64); 4] {
    [
        bytes.range(len),
        handles.range(count),
        sizes[0].range(1),
        sizes[1].range(1),
    ]
}

/// Prepares and writes only the two sizes of a received message.
fn publish_sizes(
    process: &Arc<Process>,
    sizes: &[UserOut<u64>; 2],
    len: u64,
    count: u64,
) -> Result<(), Error> {
    let outputs = prepare(process, &[sizes[0].range(1), sizes[1].range(1)])?;
    publish(outputs, &[bytes_of(&[len]), bytes_of(&[count])]);
    Ok(())
}

/// Why `channel_read` did not take the message it prepared for.
enum NotTaken {
    /// The oldest message is no longer one of the sizes prepared for.
    Changed,
    Failed(Error),
}

impl From<Error> for NotTaken {
    fn from(e: Error) -> NotTaken {
        NotTaken::Failed(e)
    }
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
    let endpoint = object::<Endpoint>(channel);
    let sizes = [out_len, out_count];
    // The outputs are prepared for the oldest message's sizes with no lock
    // held; when another reader takes it meanwhile, this one starts again.
    loop {
        let (len, count) = endpoint.front_sizes()?;
        if len > bytes_cap || count > handles_cap {
            publish_sizes(&process, &sizes, len, count)?;
            return Err(Error::BufferTooSmall);
        }
        if !bytes.fits(len) || !handles.fits(count) {
            return Err(Error::InvalidArgs);
        }
        let outputs = prepare(
            &process,
            &message_ranges(&bytes, &handles, &sizes, len, count),
        )?;
        let mut table = process.handles().lock();
        let taken = endpoint.take(|data, entries| {
            if (data.len() as u64, entries.len() as u64) != (len, count) {
                return Err(NotTaken::Changed);
            }
            Ok(crate::output::install(
                entries,
                &mut table,
                Some(process.budget()),
            )?)
        });
        drop(table);
        let mut work = Work::new(crate::sched::preempt_point);
        match taken {
            Ok((values, message)) => {
                let handles = &values[..count as usize];
                crate::output::deliver(outputs, &message.bytes, handles, &mut work);
                return Ok(());
            }
            Err(NotTaken::Changed) => outputs.release(&mut work),
            Err(NotTaken::Failed(e)) => {
                // The message stays queued.
                end_undelivered(outputs, e, len, count, &mut work);
                return Err(e);
            }
        }
    }
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
    let outputs = prepare(&current_process()?, &[buf.range(size)])?;
    if kind == STATE_DEBUG {
        let regs = thread.with_stopped_debug(|r| *r).ok_or(Error::BadState)?;
        publish(outputs, &[&thread_state::debug_bytes(&regs)]);
        return Ok(());
    }
    // The state is copied to kernel buffers while the thread is held and
    // published after it is let go.
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
        STATE_GENERAL => publish(outputs, &[&general]),
        _ => publish(outputs, &[&extended]),
    }
    Ok(())
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
    let outputs = prepare_optional(&process, &old, ACTION_SIZE as u64)?;
    let threads = process.live_threads();
    let mut state = process.signals().lock();
    let slot = signo as usize - 1;
    let previous = state.actions[slot].bytes();
    if let Some(new) = new {
        state.actions[slot] = new;
        if new.ignores(signo) {
            crate::signal::discard(&mut state, &threads, sig::bit(signo));
        }
    }
    drop(state);
    publish_optional(outputs, &old, &previous);
    Ok(())
}

/// Prepares the output of `len` values at `out`, which has been checked
/// to fit, unless its address is zero, in which case nothing is prepared
/// and publishing writes nothing.
fn prepare_optional<T: super::Plain>(
    process: &Arc<Process>,
    out: &UserOut<T>,
    len: u64,
) -> Result<Outputs<Held>, Error> {
    match out.is_null() {
        true => prepare(process, &[(0, 0)]),
        false => prepare(process, &[out.range(len)]),
    }
}

pub fn signal_set_mask(how: u32, mask: u64, old: UserOut<u64>) -> Result<(), Error> {
    if how > 2 {
        return Err(Error::InvalidArgs);
    }
    if !old.is_null() && !old.fits(1) {
        return Err(Error::InvalidArgs);
    }
    let outputs = prepare_optional(&current_process()?, &old, 1)?;
    let thread = current_thread()?;
    let mut state = thread.signals().lock();
    let previous = state.mask;
    let mask = mask & !UNBLOCKABLE;
    state.mask = match how {
        0 => state.mask | mask,
        1 => state.mask & !mask,
        _ => mask,
    };
    drop(state);
    publish_optional(outputs, &old, bytes_of(&[previous]));
    Ok(())
}

/// Publishes `bytes` to an output `prepare_optional` prepared for `out`.
fn publish_optional<T: super::Plain>(outputs: Outputs<Held>, out: &UserOut<T>, bytes: &[u8]) {
    match out.is_null() {
        true => publish(outputs, &[]),
        false => publish(outputs, &[bytes]),
    }
}

/// The minimum size of an alternate signal stack.
const ALTSTACK_MIN: u64 = 16384;

pub fn signal_set_altstack(base: u64, size: u64, old: UserOut<u64>) -> Result<(), Error> {
    let remove = base == 0 && size == 0;
    if !remove && (size < ALTSTACK_MIN || !crate::aspace::in_user_range(base, size)) {
        return Err(Error::InvalidArgs);
    }
    if !old.is_null() && !old.fits(2) {
        return Err(Error::InvalidArgs);
    }
    let outputs = prepare_optional(&current_process()?, &old, 2)?;
    let thread = current_thread()?;
    let mut state = thread.signals().lock();
    if state.on_altstack(crate::percpu::user_rsp()) {
        return Err(Error::BadState);
    }
    let (b, s) = state.altstack;
    state.altstack = (base, size);
    drop(state);
    publish_optional(outputs, &old, bytes_of(&[b, s]));
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
    if !out.fits(1) {
        return Err(Error::InvalidArgs);
    }
    let thread = current_thread()?;
    let process = current_process()?;
    let outputs = prepare(&process, &[out.range(1)])?;
    publish(
        outputs,
        &[bytes_of(&[crate::signal::pending_mask(&process, &thread)])],
    );
    Ok(())
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
    let outputs = prepare(&current_process()?, &[out.range(RECORD_SIZE as u64)])?;
    let record = crate::log::next_after(after).ok_or(Error::ShouldWait)?;
    publish(outputs, &[&record.bytes()]);
    Ok(())
}

pub fn log_console_release(_log: Checked) -> Result<(), Error> {
    crate::console::release();
    Ok(())
}

pub fn clock_get_monotonic(out: UserOut<i64>) -> Result<(), Error> {
    if !out.fits(1) {
        return Err(Error::InvalidArgs);
    }
    let outputs = prepare(&current_process()?, &[out.range(1)])?;
    publish(outputs, &[bytes_of(&[crate::time::now()])]);
    Ok(())
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
    let outputs = prepare(&process, &[observed.range(1)])?;
    let current = handle.object.header().signals();
    publish(outputs, &[bytes_of(&[state.unwrap_or(current)])]);
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

pub fn port_create(options: u32, out: UserOut<Handle>) -> Result<(), Error> {
    if options != 0 || !out.fits(1) {
        return Err(Error::InvalidArgs);
    }
    let port = crate::port::Port::new(current_process()?.budget())?;
    give(port, ObjectType::Port, out)
}

pub fn object_wait_async(
    object: Checked,
    port: Checked,
    key: u64,
    signals: u32,
    options: u32,
) -> Result<(), Error> {
    let process = current_process()?;
    let port = self::object::<crate::port::Port>(port);
    crate::port::wait_async(
        &object.object,
        &process,
        object.value,
        &port,
        key,
        signals,
        options,
    )
}

pub fn port_wait(port: Checked, deadline: i64, out: UserOut<u8>) -> Result<(), Error> {
    if !out.fits(crate::port::PACKET_SIZE as u64) {
        return Err(Error::InvalidArgs);
    }
    let process = current_process()?;
    let port = object::<crate::port::Port>(port);
    // The packet leaves the queue only once its output is prepared; when
    // another waiter takes it meanwhile, this one waits again.
    loop {
        port.wait_queued(deadline)?;
        let outputs = prepare(&process, &[out.range(crate::port::PACKET_SIZE as u64)])?;
        if let Some(packet) = port.take() {
            publish(outputs, &[&packet]);
            return Ok(());
        }
        drop(outputs);
    }
}

pub fn port_queue(port: Checked, packet: UserIn<u8>) -> Result<(), Error> {
    let mut bytes = [0u8; crate::port::PACKET_SIZE];
    packet.read_bytes(&mut bytes)?;
    let process = current_process()?;
    object::<crate::port::Port>(port).queue_user(&bytes, process.budget())
}

pub fn port_cancel(port: Checked, object: Checked, key: u64) -> Result<(), Error> {
    let process = current_process()?;
    let value = object.value;
    let port = self::object::<crate::port::Port>(port);
    crate::port::cancel(&port, Arc::as_ptr(&process) as usize, value, key);
    Ok(())
}

pub fn log_reader_create(_log: Checked, after: u64, out: UserOut<Handle>) -> Result<(), Error> {
    if !out.fits(1) {
        return Err(Error::InvalidArgs);
    }
    let reader = crate::log::LogReader::new(current_process()?.budget(), after)?;
    give(reader, ObjectType::LogReader, out)
}

pub fn log_reader_read(reader: Checked, out: UserOut<u8>) -> Result<(), Error> {
    if !out.fits(RECORD_SIZE as u64) {
        return Err(Error::InvalidArgs);
    }
    let reader = object::<crate::log::LogReader>(reader);
    let record = reader.next()?;
    // The position moves only once the record has been copied out.
    out.write_bytes(&record.bytes())?;
    reader.advance(record.sequence);
    Ok(())
}

pub fn mem_op(mem: Checked, op: u32, offset: u64, len: u64) -> Result<(), Error> {
    let mem = object::<MemoryObject>(mem);
    match op {
        1 => mem.commit(offset, len),
        2 => mem.decommit(offset, len),
        _ => Err(Error::InvalidArgs),
    }
}

pub fn system_get_memory_info(out: UserOut<u8>) -> Result<(), Error> {
    if !out.fits(16) {
        return Err(Error::InvalidArgs);
    }
    let outputs = prepare(&current_process()?, &[out.range(16)])?;
    let (total, free) = match crate::memory::FRAMES.lock().as_ref() {
        Some(frames) => (frames.total_count(), frames.free_count()),
        None => (0, 0),
    };
    let mut record = [0u8; 16];
    record[0..8].copy_from_slice(&total.to_le_bytes());
    record[8..16].copy_from_slice(&free.to_le_bytes());
    publish(outputs, &[&record]);
    Ok(())
}

pub fn vm_protect(process: Checked, options: u32, addr: u64, len: u64) -> Result<(), Error> {
    let perms = MapOptions::parse_protection(options)?;
    let end = page_range_end(addr, len)?;
    if !crate::aspace::in_user_range(addr, end - addr) {
        return Err(Error::NotFound);
    }
    let writes = perms & kernel_core::regions::PERM_WRITE != 0;
    object::<Process>(process)
        .aspace()
        .protect(addr, end, perms, |r| {
            r.mem.check_revoked().is_ok() && (!writes || r.mem.check_writable().is_ok())
        })
}

pub fn mem_seal(mem: Checked, seals: u32) -> Result<(), Error> {
    object::<MemoryObject>(mem).seal(seals)
}

pub fn futex_wait(addr: UserIn<u32>, expected: u32, deadline: i64) -> Result<(), Error> {
    crate::wait::futex_wait(addr.0, expected, deadline)
}

pub fn futex_wake(addr: UserIn<u32>, count: u32, woken: UserOut<u32>) -> Result<(), Error> {
    if !woken.fits(1) {
        return Err(Error::InvalidArgs);
    }
    let outputs = prepare(&current_process()?, &[woken.range(1)])?;
    let n = crate::wait::futex_wake(addr.0, count)?;
    publish(outputs, &[bytes_of(&[n])]);
    Ok(())
}

pub fn timeline_create(initial: u64, options: u32, out: UserOut<Handle>) -> Result<(), Error> {
    if options != 0 || !out.fits(1) {
        return Err(Error::InvalidArgs);
    }
    let timeline = crate::timeline::Timeline::new(current_process()?.budget(), initial)?;
    give(timeline, ObjectType::Timeline, out)
}

pub fn timeline_signal(timeline: Checked, value: u64) -> Result<(), Error> {
    object::<crate::timeline::Timeline>(timeline).signal(value)
}

pub fn timeline_get(timeline: Checked, out: UserOut<u64>) -> Result<(), Error> {
    if !out.fits(1) {
        return Err(Error::InvalidArgs);
    }
    let outputs = prepare(&current_process()?, &[out.range(1)])?;
    let value = object::<crate::timeline::Timeline>(timeline).value();
    publish(outputs, &[bytes_of(&[value])]);
    Ok(())
}

pub fn timeline_wait(timeline: Checked, value: u64, deadline: i64) -> Result<(), Error> {
    let process = current_process()?;
    object::<crate::timeline::Timeline>(timeline).wait(&process, value, deadline)
}

pub fn timeline_wait_async(
    timeline: Checked,
    port: Checked,
    key: u64,
    value: u64,
) -> Result<(), Error> {
    let process = current_process()?;
    let current = downcast::<crate::timeline::Timeline>(timeline.object.clone())
        .expect("the dispatch checked the object type")
        .value();
    let port = object::<crate::port::Port>(port);
    crate::port::wait_timeline(
        &timeline.object,
        current,
        &process,
        timeline.value,
        &port,
        key,
        value,
    )
}

pub fn stream_create(
    options: u32,
    out0: UserOut<Handle>,
    out1: UserOut<Handle>,
) -> Result<(), Error> {
    if options != 0 || !out0.fits(1) || !out1.fits(1) {
        return Err(Error::InvalidArgs);
    }
    let process = current_process()?;
    let (a, b) = StreamEnd::pair(Some(process.budget()))?;
    give_pair([a, b], default_rights(ObjectType::Stream), [out0, out1])
}

/// A kernel buffer for up to one queue's worth of the `len` bytes of a
/// stream call.
fn stream_buffer(len: u64) -> Result<Vec<u8>, Error> {
    let n = len.min(STREAM_QUEUE_BYTES as u64) as usize;
    let mut buf = Vec::new();
    buf.try_reserve_exact(n).map_err(|_| Error::NoMemory)?;
    buf.resize(n, 0);
    Ok(buf)
}

pub fn stream_write(
    stream: Checked,
    bytes: UserIn<u8>,
    len: u64,
    actual: UserOut<u64>,
) -> Result<(), Error> {
    if !actual.fits(1) || !bytes.fits(len) {
        return Err(Error::InvalidArgs);
    }
    let mut buf = stream_buffer(len)?;
    bytes.read_bytes(&mut buf)?;
    let outputs = prepare(&current_process()?, &[actual.range(1)])?;
    let written = object::<StreamEnd>(stream).write(&buf)? as u64;
    publish(outputs, &[bytes_of(&[written])]);
    Ok(())
}

pub fn stream_read(
    stream: Checked,
    bytes: UserOut<u8>,
    len: u64,
    actual: UserOut<u64>,
) -> Result<(), Error> {
    if !actual.fits(1) || !bytes.fits(len) {
        return Err(Error::InvalidArgs);
    }
    let mut buf = stream_buffer(len)?;
    // Every byte the read could return has backing before any leaves the
    // queue.
    let ranges = [bytes.range(buf.len() as u64), actual.range(1)];
    let outputs = prepare(&current_process()?, &ranges)?;
    let n = object::<StreamEnd>(stream).read(&mut buf)?;
    publish(outputs, &[&buf[..n], bytes_of(&[n as u64])]);
    Ok(())
}

pub fn mem_clone(
    mem: Checked,
    options: u32,
    offset: u64,
    len: u64,
    out: UserOut<Handle>,
) -> Result<(), Error> {
    if options & !kernel_core::object::memory::RESIZABLE != 0 || !out.fits(1) {
        return Err(Error::InvalidArgs);
    }
    let rights = mem.rights;
    let source = object::<MemoryObject>(mem);
    if source.is_physical() {
        return Err(Error::NotSupported);
    }
    source.check_range(offset, len)?;
    let process = current_process()?;
    let budget = process.budget();
    let clone = MemoryObject::new(budget, len, options)?;
    let mut work = Work::new(crate::sched::preempt_point);
    let held = source.hold_for_read(offset, len, Some(budget), &mut work)?;
    // The bytes are copied at once, in pieces that stay within one page
    // of each object; pieces that read as zero are left without a frame.
    let mut piece = [0u8; PAGE_SIZE as usize];
    let mut done = 0;
    let mut copied = Ok(());
    while done < len && copied.is_ok() {
        let at = offset + done;
        let n = (PAGE_SIZE - at % PAGE_SIZE)
            .min(PAGE_SIZE - done % PAGE_SIZE)
            .min(len - done) as usize;
        let bytes = &mut piece[..n];
        held.read(at, bytes);
        if bytes.iter().any(|&b| b != 0) {
            let mut rest: &[u8] = bytes;
            copied = clone.write(done, n as u64, |to| {
                let (now, later) = rest.split_at(to.len());
                to.copy_from_slice(now);
                rest = later;
                Ok(())
            });
        }
        done += n as u64;
        work.item();
    }
    held.release(&mut work);
    copied?;
    give_with(clone, rights, out)
}

pub fn pager_create(options: u32, out: UserOut<Handle>) -> Result<(), Error> {
    if options != 0 || !out.fits(1) {
        return Err(Error::InvalidArgs);
    }
    let pager = crate::pager::Pager::new(current_process()?.budget())?;
    give(pager, ObjectType::Pager, out)
}

pub fn pager_create_memory(
    pager: Checked,
    port: Checked,
    key: u64,
    size: u64,
    options: u32,
    out: UserOut<Handle>,
) -> Result<(), Error> {
    if !out.fits(1) {
        return Err(Error::InvalidArgs);
    }
    let pager = object::<crate::pager::Pager>(pager);
    let port = object::<crate::port::Port>(port);
    let budget = current_process()?.budget().clone();
    let mem = MemoryObject::new_paged(&budget, pager.header().koid(), &port, key, size, options)?;
    pager.add(&mem)?;
    give(mem, ObjectType::MemoryObject, out)
}

/// The paged object `mem` of `pager`, or `AccessDenied` when `pager` is
/// not its pager and `NotSupported` when it is not paged.
fn paged_of(pager: Checked, mem: Checked) -> Result<Arc<MemoryObject>, Error> {
    let mem = object::<MemoryObject>(mem);
    if !mem.is_paged() {
        return Err(Error::NotSupported);
    }
    if !mem.paged_by(&*pager.object) {
        return Err(Error::AccessDenied);
    }
    Ok(mem)
}

pub fn pager_supply(
    pager: Checked,
    mem: Checked,
    offset: u64,
    len: u64,
    source: Checked,
    source_offset: u64,
) -> Result<(), Error> {
    let mem = paged_of(pager, mem)?;
    let source = object::<MemoryObject>(source);
    mem.supply(offset, len, &source, source_offset)
}

pub fn pager_op(pager: Checked, mem: Checked, op: u32, offset: u64, len: u64) -> Result<(), Error> {
    paged_of(pager, mem)?.pager_op(op, offset, len)
}

pub fn pager_query_dirty(
    pager: Checked,
    mem: Checked,
    offset: u64,
    len: u64,
    out: UserOut<u8>,
    cap: u64,
    count: UserOut<u64>,
) -> Result<(), Error> {
    if !count.fits(1) {
        return Err(Error::InvalidArgs);
    }
    let ranges = paged_of(pager, mem)?.dirty_ranges(offset, len)?;
    let n = (ranges.len() as u64).min(cap);
    if !out.fits(n * 16) {
        return Err(Error::InvalidArgs);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(n as usize * 16)
        .map_err(|_| Error::NoMemory)?;
    for &(at, length) in ranges.iter().take(n as usize) {
        bytes.extend_from_slice(&at.to_le_bytes());
        bytes.extend_from_slice(&length.to_le_bytes());
    }
    let outputs = prepare(&current_process()?, &[out.range(n * 16), count.range(1)])?;
    publish(outputs, &[&bytes, bytes_of(&[n])]);
    Ok(())
}

pub fn pager_revoke(pager: Checked, mem: Checked) -> Result<(), Error> {
    paged_of(pager, mem)?.revoke()
}
