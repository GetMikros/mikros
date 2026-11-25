//! What a file holds: a resizable anonymous memory object, made on the
//! first write and charged to the budget of the nearest directory above
//! the file that has one, or the server's own, and the file's size,
//! which the object covers rounded up to a page. Clients get a paged
//! object of the file instead, made when one is first asked for, whose
//! pages are those of the file's object.

use crate::service::fs::Fs;
use crate::service::paged::Paged;
use alloc::rc::Rc;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::sys;

const RESIZABLE: u32 = 1 << 0;
const PAGE: u64 = 4096;
const WRITE: u32 = 1 << 3;
const EXECUTE: u32 = 1 << 4;

#[derive(Default)]
pub struct File {
    pub mem: Option<Rc<OwnedHandle>>,
    /// The object that clients map, once one was asked for.
    paged: Option<Rc<Paged>>,
    pub size: u64,
    /// The budget to charge the object to when it is made.
    pub charge: Option<Rc<OwnedHandle>>,
}

/// Why changing a file failed.
pub enum Failed {
    /// The kernel refused memory.
    NoSpace,
    Io,
    /// The file is run from an object that is in use.
    Busy,
}

impl Drop for File {
    fn drop(&mut self) {
        if let Some(paged) = &self.paged {
            paged.release();
        }
    }
}

impl File {
    /// The bytes from `offset`, at most `count`.
    pub fn read(&self, offset: u64, count: usize) -> Result<alloc::vec::Vec<u8>, Failed> {
        let end = offset.saturating_add(count as u64).min(self.size);
        let Some(mem) = &self.mem else {
            return Ok(alloc::vec::Vec::new());
        };
        if offset >= end {
            return Ok(alloc::vec::Vec::new());
        }
        let mut data = alloc::vec![0u8; (end - offset) as usize];
        // SAFETY: the pointer and length describe `data`.
        let status =
            unsafe { sys::mem_read(mem.raw(), offset, data.as_mut_ptr(), data.len() as u64) };
        match status {
            mikros_rt::OK => Ok(data),
            _ => Err(Failed::Io),
        }
    }

    /// Makes the object cover `size` bytes, creating it when the file has
    /// none.
    fn reserve(&mut self, size: u64) -> Result<(), Failed> {
        let capacity = size.next_multiple_of(PAGE).max(PAGE);
        match &self.mem {
            None => {
                let mut raw = 0;
                // SAFETY: the out pointer is valid for the calls.
                let status = unsafe {
                    match self.charge.take() {
                        Some(budget) => {
                            sys::mem_create_charged(budget.raw(), capacity, RESIZABLE, &mut raw)
                        }
                        None => sys::mem_create(capacity, RESIZABLE, &mut raw),
                    }
                };
                if status != mikros_rt::OK {
                    return Err(Failed::NoSpace);
                }
                // SAFETY: the kernel just created the handle for us.
                self.mem = Some(Rc::new(unsafe { OwnedHandle::from_raw(raw) }));
                Ok(())
            }
            Some(mem) if capacity > self.size.next_multiple_of(PAGE).max(PAGE) => {
                // SAFETY: the call takes no pointers.
                match unsafe { sys::mem_set_size(mem.raw(), capacity) } {
                    mikros_rt::OK => {
                        // The pages of the new part can be asked for now.
                        match &self.paged {
                            Some(paged) if !paged.resize(capacity) => Err(Failed::Io),
                            _ => Ok(()),
                        }
                    }
                    _ => Err(Failed::NoSpace),
                }
            }
            Some(_) => Ok(()),
        }
    }

    /// Writes `data` at `offset`, growing the file, with zeros between
    /// its old end and `offset`.
    pub fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), Failed> {
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or(Failed::NoSpace)?;
        if data.is_empty() {
            return Ok(());
        }
        self.not_run()?;
        self.reserve(end.max(self.size))?;
        let mem = self.mem.as_ref().ok_or(Failed::Io)?;
        // SAFETY: the pointer and length describe `data`.
        let status = unsafe { sys::mem_write(mem.raw(), offset, data.as_ptr(), data.len() as u64) };
        if status != mikros_rt::OK {
            return Err(Failed::Io);
        }
        self.size = self.size.max(end);
        Ok(())
    }

    /// Sets the size to `size`: the bytes past it are gone, and those it
    /// gains read as zero.
    pub fn truncate(&mut self, size: u64) -> Result<(), Failed> {
        self.not_run()?;
        if size > self.size {
            self.reserve(size)?;
        } else if let Some(mem) = &self.mem {
            let capacity = size.next_multiple_of(PAGE).max(PAGE);
            let before = self.size.next_multiple_of(PAGE).max(PAGE);
            // What the clients' object lost goes before the file's does.
            if self.paged.as_ref().is_some_and(|p| !p.resize(capacity)) {
                return Err(Failed::Io);
            }
            // SAFETY: the call takes no pointers.
            if unsafe { sys::mem_set_size(mem.raw(), capacity) } != mikros_rt::OK {
                // The file keeps its size, and so does the clients' object.
                if let Some(paged) = &self.paged {
                    paged.resize(before);
                }
                return Err(Failed::Io);
            }
            // The rest of the last page reads as zero once the file grows.
            let tail = (capacity - size) as usize;
            if tail > 0 {
                let zeros = alloc::vec![0u8; tail];
                // SAFETY: the pointer and length describe `zeros`.
                unsafe { sys::mem_write(mem.raw(), size, zeros.as_ptr(), tail as u64) };
            }
        }
        self.size = size;
        Ok(())
    }

    /// Fails when the file is run from an object that is in use, as a
    /// change would reach the running program.
    fn not_run(&self) -> Result<(), Failed> {
        match self.paged.as_ref().is_some_and(|p| p.busy_for_change()) {
            true => Err(Failed::Busy),
            false => Ok(()),
        }
    }

    /// A handle with `rights` to the file's paged object, made empty
    /// when the file has none yet, which shows every later change and
    /// whose changes are the file's. An object to run from fails while
    /// one that was given with `WRITE` is in use, and one to write while
    /// the file is run from.
    pub fn shared(&mut self, rights: u32, fs: &Fs) -> Result<OwnedHandle, Failed> {
        let execute = rights & EXECUTE != 0;
        let write = rights & WRITE != 0;
        if write {
            self.not_run()?;
        }
        if execute && self.paged.as_ref().is_some_and(|p| p.busy_for_execute()) {
            return Err(Failed::Busy);
        }
        self.reserve(self.size)?;
        let paged = match &self.paged {
            Some(paged) => paged.clone(),
            None => {
                let pager = fs.pager.as_ref().ok_or(Failed::Io)?;
                let backing = self.mem.clone().ok_or(Failed::Io)?;
                let len = self.size.next_multiple_of(PAGE).max(PAGE);
                let paged = Paged::create(pager, &fs.reactor, &fs.spawner, backing, len)?;
                self.paged = Some(paged.clone());
                paged
            }
        };
        paged.handle(rights, execute, write)
    }

    /// A copy-on-write clone of the file's memory object, with `rights`,
    /// whose changes stay its own.
    pub fn private(&mut self, rights: u32) -> Result<OwnedHandle, Failed> {
        self.reserve(self.size)?;
        let mem = self.mem.as_ref().ok_or(Failed::Io)?;
        let len = self.size.next_multiple_of(PAGE).max(PAGE);
        let mut raw = 0;
        // SAFETY: the out pointer is valid for the call.
        if unsafe { sys::mem_clone(mem.raw(), 0, 0, len, &mut raw) } != mikros_rt::OK {
            return Err(Failed::NoSpace);
        }
        let mut out = 0;
        // SAFETY: the out pointer is valid for the call; the kernel closes
        // the old handle.
        match unsafe { sys::handle_replace(raw, rights, &mut out) } {
            // SAFETY: the kernel just created the handle for us.
            mikros_rt::OK => Ok(unsafe { OwnedHandle::from_raw(out) }),
            _ => Err(Failed::Io),
        }
    }

    /// The bytes the object takes.
    pub fn allocated(&self) -> u64 {
        match &self.mem {
            Some(_) => self.size.next_multiple_of(PAGE).max(PAGE),
            None => 0,
        }
    }
}
