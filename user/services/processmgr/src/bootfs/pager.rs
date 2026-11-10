//! The thread that answers the page requests of the boot files' paged
//! objects. It reads only the boot image, which stays mapped, so it never
//! waits for the executor. The executor may itself wait for a page of a
//! file that another server supplies, and that server's code may consist
//! of pages of boot files: answered on the executor, those would never
//! come.

use super::memory::copy_bytes;
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use mikros_rt::sys::{self, Handle, Status};

/// The `PAGER` packet type and its `READ` command, and the pager op that
/// fails pages.
const PACKET_PAGER: u32 = 3;
const PAGER_READ: u32 = 1;
const OP_FAIL: u32 = 1;
/// The bytes of a port packet.
const PACKET_SIZE: usize = 48;
const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
/// The thread's stack: it calls little more than the kernel.
const STACK_SIZE: u64 = 64 << 10;
const NAME: &[u8] = b"bootfs-pager";

/// A file of the boot image: where it is, and its paged object once made.
struct File {
    offset: usize,
    size: usize,
    /// The paged object's handle, 0 until it is made.
    mem: AtomicU32,
}

/// The pager of the boot files, its port, and the files by node.
pub(super) struct Pager {
    pager: Handle,
    port: Handle,
    image: &'static [u8],
    files: Box<[File]>,
}

impl Pager {
    /// Makes the pager and the port for the boot image `image`, whose
    /// files are at `files` by node (empty for nodes that are not files),
    /// and starts the thread of `process` that answers the requests. The
    /// pager lasts as long as the process does.
    pub(super) fn start(
        process: Handle,
        image: &'static [u8],
        files: Vec<(usize, usize)>,
    ) -> Result<&'static Pager, Status> {
        let (mut pager, mut port) = (0, 0);
        // SAFETY: the out pointers are valid for the calls.
        unsafe {
            check(sys::pager_create(0, &mut pager))?;
            if let Err(status) = check(sys::port_create(0, &mut port)) {
                sys::handle_close(pager);
                return Err(status);
            }
        }
        let files = files
            .into_iter()
            .map(|(offset, size)| File {
                offset,
                size,
                mem: AtomicU32::new(0),
            })
            .collect();
        let this: &'static Pager = Box::leak(Box::new(Pager {
            pager,
            port,
            image,
            files,
        }));
        spawn(process, this)?;
        Ok(this)
    }

    /// The paged object of the file `node`, of `len` bytes, a page
    /// multiple, made the first time.
    pub(super) fn object(&self, node: usize, len: u64) -> Result<Handle, Status> {
        let file = &self.files[node];
        let mem = file.mem.load(Ordering::Acquire);
        if mem != 0 {
            return Ok(mem);
        }
        let mut raw = 0;
        // SAFETY: the out pointer is valid for the call.
        check(unsafe {
            sys::pager_create_memory(self.pager, self.port, node as u64, len, 0, &mut raw)
        })?;
        // Nobody has a handle to it yet, so no request for it comes
        // before this store.
        file.mem.store(raw, Ordering::Release);
        Ok(raw)
    }

    /// Answers the page requests for as long as the process runs.
    fn answer(&self) -> ! {
        let word =
            |p: &[u8], at: usize| u64::from_le_bytes(p[at..at + 8].try_into().expect("8 bytes"));
        let half =
            |p: &[u8], at: usize| u32::from_le_bytes(p[at..at + 4].try_into().expect("4 bytes"));
        loop {
            let mut packet = [0u8; PACKET_SIZE];
            // SAFETY: the pointer describes `packet`, a whole packet.
            if unsafe { sys::port_wait(self.port, i64::MAX, packet.as_mut_ptr()) } != mikros_rt::OK
            {
                continue;
            }
            if half(&packet, 8) != PACKET_PAGER || half(&packet, 16) != PAGER_READ {
                continue;
            }
            let (node, at, len) = (word(&packet, 0), word(&packet, 24), word(&packet, 32));
            let Some(file) = self.files.get(node as usize) else {
                continue;
            };
            let mem = file.mem.load(Ordering::Acquire);
            if mem == 0 {
                continue;
            }
            let start = (at as usize).min(file.size);
            let count = (len as usize).min(file.size - start);
            let bytes = &self.image[file.offset + start..file.offset + start + count];
            let supplied = copy_bytes(bytes, len).and_then(|source| {
                // SAFETY: the call takes no pointers.
                check(unsafe { sys::pager_supply(self.pager, mem, at, len, source.raw(), 0) })
            });
            if let Err(status) = supplied {
                mikros_rt::log!(Error, "supplying a page of a boot file failed: {status}");
                // The access fails rather than waits for good.
                // SAFETY: the call takes no pointers.
                unsafe { sys::pager_op(self.pager, mem, OP_FAIL, at, len) };
            }
        }
    }
}

fn check(status: Status) -> Result<(), Status> {
    match status {
        mikros_rt::OK => Ok(()),
        status => Err(status),
    }
}

/// Starts the thread of `process` that answers the requests of `pager`.
fn spawn(process: Handle, pager: &'static Pager) -> Result<(), Status> {
    let mut stack = 0;
    // SAFETY: the out pointer is valid for the call.
    check(unsafe { sys::mem_create(STACK_SIZE, 0, &mut stack) })?;
    let mut low = 0;
    // SAFETY: the out pointer is valid; the kernel picks a free range.
    let mapped = unsafe {
        sys::vm_map(
            process,
            PERM_READ | PERM_WRITE,
            0,
            stack,
            0,
            STACK_SIZE,
            &mut low,
        )
    };
    // The mapping keeps the stack's memory.
    // SAFETY: closing takes no pointers.
    unsafe { sys::handle_close(stack) };
    check(mapped)?;
    let mut thread = 0;
    // SAFETY: the name and the out pointer are valid for the call.
    let created =
        unsafe { sys::thread_create(process, NAME.as_ptr(), NAME.len() as u64, &mut thread) };
    if let Err(status) = check(created) {
        // SAFETY: nothing uses the range.
        unsafe { sys::vm_unmap(process, low, STACK_SIZE) };
        return Err(status);
    }
    // SAFETY: the stack is mapped and unused, 8 bytes below a 16-byte
    // boundary as a function's entry expects; the pager lives for good.
    let started = unsafe {
        sys::thread_start(
            thread,
            run as *const () as u64,
            low + STACK_SIZE - 8,
            pager as *const Pager as u64,
            0,
        )
    };
    // SAFETY: closing takes no pointers; the thread runs without it.
    unsafe { sys::handle_close(thread) };
    check(started)
}

extern "sysv64" fn run(pager: u64, _: u64) -> ! {
    // SAFETY: `spawn` passes a pager that lives for good.
    let pager = unsafe { &*(pager as *const Pager) };
    pager.answer()
}
