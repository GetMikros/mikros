//! Starting processes: loading the program, which the spawner's
//! namespace names, and sending the startup transfer on its bootstrap
//! channel.

use crate::namespace::{self, Origin, Programs};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_manifest::path;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::{MemoryFlags, Node, NodeType, OpenFlags, Rights};
use mikros_rt::interfaces::mikros::process::{
    Bootstrap, BootstrapClient, Launcher, NamespaceEntry, StartupHandle, StartupHeader,
};
use mikros_rt::interfaces::mikros::service::Directory;
use mikros_rt::sys::{self, Handle};
use process_manager::bootfs::{Executable, Fs};

const PAGE_SIZE: u64 = 4096;
const PERM_READ: u32 = 1 << 0;
/// The only version of the transfer.
const VERSION: u32 = 1;
/// The most entries one `Handles` or `Namespace` batch carries.
const BATCH_MAX: usize = 32;
const SAME_RIGHTS: u32 = 1 << 31;
/// Handle rights of the clock page. `TRANSFER` lets it be sent and
/// `INSPECT` lets the receiver check its type; the receiver keeps
/// `READ` and `MAP`.
const TRANSFER: u32 = 1 << 1;
const READ: u32 = 1 << 2;
const MAP: u32 = 1 << 5;
const INSPECT: u32 = 1 << 9;

pub fn status(raw: sys::Status) -> Status {
    Status::from_raw(raw as i32)
}

fn check(raw: sys::Status) -> Result<(), Status> {
    match raw {
        mikros_rt::OK => Ok(()),
        raw => Err(status(raw)),
    }
}

/// A second handle to `handle`'s object with `rights`.
pub fn duplicate(handle: Handle, rights: u32) -> Result<OwnedHandle, Status> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    check(unsafe { sys::handle_duplicate(handle, rights, &mut out) })?;
    // SAFETY: the handle was just created for us.
    Ok(unsafe { OwnedHandle::from_raw(out) })
}

fn same(handle: &OwnedHandle) -> Result<OwnedHandle, Status> {
    duplicate(handle.raw(), SAME_RIGHTS)
}

/// A process that runs its program and waits for its startup transfer.
pub struct Started {
    pub process: OwnedHandle,
    pub thread: OwnedHandle,
    bootstrap: BootstrapClient,
    base: u64,
    phdr: u64,
    phnum: u32,
    /// Where the dynamic linker was loaded, or 0.
    interpreter_base: u64,
}

impl Started {
    /// Ends the process, for a start that failed half-way.
    pub fn kill(&self) {
        // SAFETY: the call takes no pointers.
        unsafe { sys::task_kill(self.process.raw()) };
    }
}

fn loading(e: mikros_loader::Error) -> Status {
    match e {
        mikros_loader::Error::Status(raw) => status(raw),
        _ => Status::InvalidArgs,
    }
}

/// Where an image was loaded, and whether it names the dynamic linker.
struct Image {
    base: u64,
    entry: u64,
    phdr: u64,
    phnum: u32,
    dynamic: bool,
    /// The start and length of the executable segment.
    code: Option<(u64, u64)>,
}

/// A file on a file system server, mapped read-only into the process
/// manager while it is loaded.
struct Mapped {
    memory: OwnedHandle,
    at: u64,
    size: u64,
    /// The mapping's length, a whole number of pages.
    len: u64,
    /// The process manager's own process.
    own: Handle,
}

impl Mapped {
    fn file(&self) -> &[u8] {
        // SAFETY: the file is mapped readable at `at` for `size` bytes
        // until `self` drops.
        unsafe { core::slice::from_raw_parts(self.at as *const u8, self.size as usize) }
    }

    /// A new memory object of `len` bytes, a whole number of pages,
    /// private to its holder, with the file's bytes from `from` and zeros
    /// after the end of the file.
    fn private_range(&self, from: u64, len: u64) -> Result<OwnedHandle, sys::Status> {
        let file = self.file();
        let start = usize::try_from(from).map_or(file.len(), |f| f.min(file.len()));
        let count = usize::try_from(len).map_or(file.len() - start, |l| l.min(file.len() - start));
        let mut mem = 0;
        // SAFETY: the out pointer is valid for the call.
        let status = unsafe { sys::mem_create(len, 0, &mut mem) };
        if status != mikros_rt::OK {
            return Err(status);
        }
        // SAFETY: the handle was just created for us.
        let mem = unsafe { OwnedHandle::from_raw(mem) };
        // SAFETY: the pointer describes `count` bytes of the file.
        let status = unsafe { sys::mem_write(mem.raw(), 0, file[start..].as_ptr(), count as u64) };
        if status != mikros_rt::OK {
            return Err(status);
        }
        Ok(mem)
    }
}

impl Drop for Mapped {
    fn drop(&mut self) {
        // SAFETY: the range is the mapping that `map_file` made, which
        // nothing uses once `self` drops.
        unsafe { sys::vm_unmap(self.own, self.at, self.len) };
    }
}

/// An executable as a path names it in a namespace (MIKSPEC-0034.R27): a
/// file of the boot file system, or one on a file system server, which
/// is a system component when it comes from the root's `/usr`.
enum Program {
    Boot(Executable),
    File { file: Mapped, system: bool },
}

impl Program {
    fn file(&self) -> &[u8] {
        match self {
            Program::Boot(executable) => executable.file,
            Program::File { file, .. } => file.file(),
        }
    }

    fn system(&self) -> bool {
        match self {
            Program::Boot(executable) => executable.system,
            Program::File { system, .. } => *system,
        }
    }
}

/// The program that the absolute `path` names in `programs`. Fails with
/// `NOT_FOUND` when no entry holds it, and `ACCESS_DENIED` when its entry
/// lacks `EXECUTE` or it is not a regular file. `own` is the process
/// manager's process, which a file on a server is mapped into.
async fn resolve(
    fs: &Rc<Fs>,
    programs: &Programs,
    own: Handle,
    path: &str,
    reactor: &Reactor,
) -> Result<Program, Status> {
    let normal = path::normalize(path).ok_or(Status::InvalidArgs)?;
    let (origin, rest) = programs.find(&normal).ok_or(Status::NotFound)?;
    match origin {
        Origin::None => Err(Status::AccessDenied),
        Origin::Boot(dir) => {
            let at = match (dir.is_empty(), rest.is_empty()) {
                (true, _) => String::from(rest),
                (false, true) => dir.clone(),
                (false, false) => format!("{dir}/{rest}"),
            };
            fs.executable(&at).map(Program::Boot).map_err(status)
        }
        Origin::Dir { dir, system } => {
            let name = if rest.is_empty() { "." } else { rest };
            let rights = Rights(Rights::READ.0 | Rights::EXECUTE.0);
            let file = namespace::open(dir, name, OpenFlags(0), 0, rights, reactor)
                .await
                .map_err(namespace::status)?;
            let attributes = reply(file.get_attributes().await)?.attributes;
            if attributes.r#type != NodeType::File {
                return Err(Status::AccessDenied);
            }
            let memory = reply(file.get_memory_object(MemoryFlags::EXECUTE).await)?.memory;
            let file = map_file(own, memory, attributes.size)?;
            Ok(Program::File {
                file,
                system: *system,
            })
        }
    }
}

/// The response of a call to a file system server, or the status a
/// start fails with.
fn reply<T>(
    r: Result<
        Result<T, mikros_rt::interfaces::mikros::fs::Error>,
        mikros_rt::interfaces::__midl::Error,
    >,
) -> Result<T, Status> {
    match r {
        Ok(Ok(r)) => Ok(r),
        Ok(Err(e)) => Err(namespace::status(e)),
        Err(_) => Err(Status::Internal),
    }
}

/// Maps the first `size` bytes of the file's memory object `memory`
/// read-only into the process manager's process `own`.
fn map_file(own: Handle, memory: OwnedHandle, size: u64) -> Result<Mapped, Status> {
    let len = size.max(1).next_multiple_of(PAGE_SIZE);
    let mut at = 0;
    // SAFETY: the out pointer is valid; the kernel picks a free range.
    check(unsafe { sys::vm_map(own, PERM_READ, 0, memory.raw(), 0, len, &mut at) })?;
    Ok(Mapped {
        memory,
        at,
        size,
        len,
        own,
    })
}

/// Maps `program` into `process`.
fn load_image(fs: &Rc<Fs>, process: Handle, program: &Program) -> Result<Image, Status> {
    let private = |offset: u64, len: u64| {
        let range = match program {
            Program::Boot(executable) => fs.private_range(executable, offset, len),
            Program::File { file, .. } => file.private_range(offset, len),
        };
        range
            .map(OwnedHandle::into_raw)
            .map_err(mikros_loader::Error::Status)
    };
    let shared = match program {
        Program::Boot(executable) => executable.shared.raw(),
        Program::File { file, .. } => file.memory.raw(),
    };
    let image = mikros_loader::Image {
        file: program.file(),
        shared,
        private: &private,
    };
    let loaded = mikros_loader::load(process, &image).map_err(loading)?;
    Ok(Image {
        base: loaded.base,
        entry: loaded.entry,
        phdr: loaded.phdr,
        phnum: loaded.phnum,
        dynamic: loaded.interpreter.is_some(),
        code: loaded.code,
    })
}

/// Loads the program that the absolute `path` names in `programs`, the
/// spawner's, into a new process charged to `budget`, and starts it with
/// a new bootstrap channel: at the entry of the dynamic linker, which is
/// loaded from the same namespace, when the program names one, and at
/// its own entry otherwise. `own` is the process manager's process.
pub async fn load(
    fs: &Rc<Fs>,
    programs: &Programs,
    own: Handle,
    reactor: &Reactor,
    budget: Handle,
    path: &str,
    name: &str,
) -> Result<Started, Status> {
    let executable = resolve(fs, programs, own, path, reactor).await?;
    // An application makes system calls only through the dynamic linker,
    // which is also the C library, so one that has none is refused.
    let application = !executable.system();
    if application && !mikros_loader::is_dynamic(executable.file()).map_err(loading)? {
        return Err(Status::NotSupported);
    }
    let mut process = 0;
    // SAFETY: the pointers describe `name` and `process`.
    check(unsafe { sys::process_create(budget, name.as_ptr(), name.len() as u64, &mut process) })?;
    // SAFETY: the handle was just created for us.
    let process = unsafe { OwnedHandle::from_raw(process) };
    let loaded = load_image(fs, process.raw(), &executable)?;
    // The loader accepts only the one dynamic linker.
    let (entry, interpreter_base) = if loaded.dynamic {
        let path = core::str::from_utf8(mikros_loader::INTERPRETER).expect("an ASCII path");
        let linker = resolve(fs, programs, own, path, reactor).await?;
        let linked = load_image(fs, process.raw(), &linker)?;
        // The dynamic linker is not itself linked dynamically.
        if linked.dynamic {
            return Err(Status::InvalidArgs);
        }
        if application {
            let (start, len) = linked.code.ok_or(Status::InvalidArgs)?;
            // SAFETY: the call takes no pointers.
            check(unsafe { sys::process_set_syscall_gate(process.raw(), start, len) })?;
        }
        (linked.entry, linked.base)
    } else {
        (loaded.entry, 0)
    };
    let top = mikros_loader::stack(process.raw()).map_err(loading)?;
    let mut thread = 0;
    // SAFETY: the pointers describe `name` and `thread`.
    check(unsafe {
        sys::thread_create(process.raw(), name.as_ptr(), name.len() as u64, &mut thread)
    })?;
    // SAFETY: the handle was just created for us.
    let thread = unsafe { OwnedHandle::from_raw(thread) };
    let (ours, theirs) = endpoints::<Bootstrap>().map_err(|_| Status::NoResources)?;
    let theirs = theirs.into_handle();
    // SAFETY: the call takes no pointers.
    check(unsafe { sys::process_start(process.raw(), thread.raw(), entry, top, theirs.raw(), 0) })?;
    // The handle moved into the new process's table.
    let _ = theirs.into_raw();
    Ok(Started {
        process,
        thread,
        bootstrap: BootstrapClient::new(ours),
        base: loaded.base,
        phdr: loaded.phdr,
        phnum: loaded.phnum,
        interpreter_base,
    })
}

/// The `arguments` memory object: the argument and environment counts,
/// then each argument NUL-terminated. There is no environment yet.
pub fn arguments(args: &[&str]) -> Result<OwnedHandle, Status> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(args.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    for arg in args {
        bytes.extend_from_slice(arg.as_bytes());
        bytes.push(0);
    }
    let mut mem = 0;
    let size = (bytes.len() as u64).next_multiple_of(PAGE_SIZE);
    // SAFETY: the out pointer is valid for the call.
    check(unsafe { sys::mem_create(size, 0, &mut mem) })?;
    // SAFETY: the handle was just created for us.
    let mem = unsafe { OwnedHandle::from_raw(mem) };
    // SAFETY: the pointer describes `bytes`.
    check(unsafe { sys::mem_write(mem.raw(), 0, bytes.as_ptr(), bytes.len() as u64) })?;
    Ok(mem)
}

/// What the startup transfer of a process carries besides its loaded
/// program.
pub struct Transfer {
    pub pid: u32,
    pub parent: u32,
    pub user: u32,
    pub app_id: String,
    /// The `arguments` memory object.
    pub arguments: OwnedHandle,
    pub handles: Vec<StartupHandle>,
    pub launcher: ClientEnd<Launcher>,
    pub directory: ClientEnd<Directory>,
    pub clock_page: Handle,
    /// The time page, when the time service gave it.
    pub time_page: Option<Handle>,
    /// The namespace's entries.
    pub namespace: Vec<NamespaceEntry>,
}

impl Started {
    /// Sends `Begin`, the `Handles` and `Namespace` batches of
    /// `transfer`, but not `Complete`.
    pub fn send(&self, transfer: Transfer) -> Result<(), Status> {
        let sent = |r: Result<(), _>| r.map_err(|_| Status::PeerClosed);
        let namespace = transfer.namespace;
        let header = StartupHeader {
            version: VERSION,
            pid: transfer.pid,
            parent: transfer.parent,
            user: transfer.user,
            app_id: transfer.app_id,
            bundle: None,
            arguments: transfer.arguments,
            handle_count: transfer.handles.len() as u32,
            namespace_count: namespace.len() as u32,
            process: same(&self.process)?,
            thread: same(&self.thread)?,
            launcher: transfer.launcher,
            directory: transfer.directory,
            time_page: transfer
                .time_page
                .map(|page| duplicate(page, TRANSFER | READ | MAP | INSPECT))
                .transpose()?,
            clock_page: duplicate(transfer.clock_page, TRANSFER | READ | MAP | INSPECT)?,
            program_base: self.base,
            program_phdr: self.phdr,
            program_phnum: self.phnum,
            interpreter_base: self.interpreter_base,
        };
        sent(self.bootstrap.begin(header))?;
        let mut handles = transfer.handles;
        let mut offset = 0;
        while !handles.is_empty() {
            let rest = handles.split_off(handles.len().min(BATCH_MAX));
            let batch = core::mem::replace(&mut handles, rest);
            let count = batch.len() as u32;
            sent(self.bootstrap.handles(offset, batch))?;
            offset += count;
        }
        sent(self.bootstrap.namespace(0, namespace))
    }

    /// Sends `Complete`, after which the process enters its program.
    pub fn complete(&self) -> Result<(), Status> {
        self.bootstrap.complete().map_err(|_| Status::PeerClosed)
    }
}

/// A connection to the root of `fs` with `READ` and `EXECUTE`.
pub fn root(fs: &Rc<Fs>) -> Result<ClientEnd<Node>, Status> {
    let (client, server) = endpoints::<Node>().map_err(|_| Status::NoResources)?;
    fs.serve_root(server, Rights::READ | Rights::EXECUTE);
    Ok(client)
}
