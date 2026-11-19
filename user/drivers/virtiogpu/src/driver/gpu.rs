//! The device: commands on its control and cursor queues, each written
//! into a pinned page of its own with room for the response, whose type
//! is checked, and 2D resources whose backing is pinned memory.

use crate::protocol::{self, RESP_OK_NODATA};
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::poll_fn;
use core::task::{Poll, Waker};
use mikros_rt::hardware::Mapping;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::sys;
use mikros_storage::{PAGE, pages, pinned};
use mikros_virtio::{Buffer, Queue, Transport};

const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
/// How many commands may be in flight at once, a page each, and where in
/// its page a command's response goes, which bounds the command's size.
const SLOTS: usize = 16;
const RESPONSE: u64 = 2048;

/// The pages that commands use.
struct Slots {
    mapping: Mapping,
    pages: Vec<u64>,
    free: RefCell<Vec<usize>>,
    waiting: RefCell<Vec<Waker>>,
    _memory: OwnedHandle,
    _pin: OwnedHandle,
}

/// The device, as the driver uses it.
pub struct Gpu {
    pub transport: Rc<Transport>,
    control: Rc<Queue>,
    cursor: Rc<Queue>,
    dma: OwnedHandle,
    process: sys::Handle,
    slots: Slots,
    next_resource: Cell<u32>,
    next_fence: Cell<u64>,
}

/// A 2D resource with its pinned backing, mapped for the driver to read
/// and write.
pub struct Resource {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    /// Bytes per line.
    pub stride: u32,
    pub size: u64,
    pub mapping: Mapping,
    pub memory: OwnedHandle,
    _pin: OwnedHandle,
}

impl Gpu {
    /// The device of `transport`, with its control and cursor queues and
    /// pinned memory from the `DMA` resource `dma`.
    pub fn new(
        transport: Rc<Transport>,
        queues: &[Rc<Queue>],
        dma: OwnedHandle,
        process: sys::Handle,
    ) -> Result<Gpu, Status> {
        let (control, cursor) = match queues {
            [control, cursor, ..] => (control.clone(), cursor.clone()),
            _ => return Err(Status::NotSupported),
        };
        let size = PAGE * SLOTS as u64;
        let (memory, pin) = pinned(&dma, transport.bti(), size)?;
        let mapping = Mapping::new(process, PERM_READ | PERM_WRITE, memory.raw(), 0, size)
            .map_err(|_| Status::NoMemory)?;
        let slots = Slots {
            mapping,
            pages: pages(&memory, SLOTS as u64)?,
            free: RefCell::new((0..SLOTS).collect()),
            waiting: RefCell::new(Vec::new()),
            _memory: memory,
            _pin: pin,
        };
        Ok(Gpu {
            transport,
            control,
            cursor,
            dma,
            process,
            slots,
            next_resource: Cell::new(1),
            next_fence: Cell::new(1),
        })
    }

    async fn take(&self) -> usize {
        poll_fn(|cx| match self.slots.free.borrow_mut().pop() {
            Some(slot) => Poll::Ready(slot),
            None => {
                self.slots.waiting.borrow_mut().push(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }

    fn give_back(&self, slot: usize) {
        self.slots.free.borrow_mut().push(slot);
        for waker in self.slots.waiting.borrow_mut().drain(..) {
            waker.wake();
        }
    }

    /// Sends `command` on `queue` and returns the `response` bytes the
    /// device wrote. A command too large for a slot's page goes in pinned
    /// memory of its own.
    async fn send(
        &self,
        queue: &Queue,
        command: &[u8],
        response: usize,
    ) -> Result<Vec<u8>, Status> {
        let slot = self.take().await;
        let (page, at) = (self.slots.pages[slot], PAGE * slot as u64);
        let mapping = &self.slots.mapping;
        let large = if command.len() as u64 > RESPONSE {
            Some(self.large(command)?)
        } else {
            mapping.write(at, command);
            None
        };
        let mut buffers = match &large {
            Some((_, _, _, addresses)) => addresses.clone(),
            None => vec![Buffer {
                address: page,
                len: command.len() as u32,
                device_writes: false,
            }],
        };
        buffers.push(Buffer {
            address: page + RESPONSE,
            len: response as u32,
            device_writes: true,
        });
        let queue_ok = buffers.len() <= usize::from(queue.size());
        if queue_ok {
            queue.submit(&buffers).await;
        }
        let mut bytes = vec![0u8; response];
        mapping.read(at + RESPONSE, &mut bytes);
        self.give_back(slot);
        drop(large);
        match queue_ok {
            true => Ok(bytes),
            false => Err(Status::NoResources),
        }
    }

    /// Pinned memory holding `command`, its mapping and pin, and a
    /// buffer for each of its pages.
    #[allow(clippy::type_complexity)]
    fn large(
        &self,
        command: &[u8],
    ) -> Result<(OwnedHandle, OwnedHandle, Mapping, Vec<Buffer>), Status> {
        let size = (command.len() as u64).next_multiple_of(PAGE);
        let (memory, pin) = pinned(&self.dma, self.transport.bti(), size)?;
        let mapping = Mapping::new(self.process, PERM_READ | PERM_WRITE, memory.raw(), 0, size)
            .map_err(|_| Status::NoMemory)?;
        mapping.write(0, command);
        let mut left = command.len() as u64;
        let buffers = pages(&memory, size / PAGE)?
            .into_iter()
            .map(|address| {
                let len = left.min(PAGE);
                left -= len;
                Buffer {
                    address,
                    len: len as u32,
                    device_writes: false,
                }
            })
            .collect();
        Ok((memory, pin, mapping, buffers))
    }

    /// Sends `command` on the control queue and checks that the device
    /// answered with `RESP_OK_NODATA`.
    pub async fn ok(&self, command: &[u8]) -> Result<(), Status> {
        self.expect(&self.control, command).await
    }

    /// Sends `command` on the cursor queue, as `ok` does.
    pub async fn cursor(&self, command: &[u8]) -> Result<(), Status> {
        self.expect(&self.cursor, command).await
    }

    async fn expect(&self, queue: &Queue, command: &[u8]) -> Result<(), Status> {
        let response = self.send(queue, command, protocol::HEADER_SIZE).await?;
        match protocol::response_type(&response) {
            Some(RESP_OK_NODATA) => Ok(()),
            other => {
                let kind = protocol::response_type(command).unwrap_or(0);
                mikros_rt::log!(
                    Error,
                    "virtiogpu: command {kind:#x} failed with response {other:#x?}"
                );
                Err(Status::Io)
            }
        }
    }

    /// Sends `command` on the control queue and returns a response of
    /// `size` bytes, whose type its reader checks.
    pub async fn query(&self, command: &[u8], size: usize) -> Result<Vec<u8>, Status> {
        self.send(&self.control, command, size).await
    }

    /// The driver's own process, which maps memory.
    pub fn process(&self) -> sys::Handle {
        self.process
    }

    /// A fence ID that no earlier command used.
    pub fn fence(&self) -> u64 {
        let fence = self.next_fence.get();
        self.next_fence.set(fence + 1);
        fence
    }

    /// A 2D resource of `width` by `height` pixels in `format`, with
    /// pinned backing attached.
    pub async fn create(&self, width: u32, height: u32, format: u32) -> Result<Resource, Status> {
        let stride = width.checked_mul(4).ok_or(Status::InvalidArgs)?;
        let size = (u64::from(stride) * u64::from(height)).next_multiple_of(PAGE);
        let (memory, pin) = pinned(&self.dma, self.transport.bti(), size)?;
        let addresses = pages(&memory, size / PAGE)?;
        let mapping = Mapping::new(self.process, PERM_READ | PERM_WRITE, memory.raw(), 0, size)
            .map_err(|_| Status::NoMemory)?;
        let id = self.next_resource.get();
        self.next_resource.set(id + 1);
        self.ok(&protocol::resource_create_2d(id, format, width, height))
            .await?;
        if let Err(status) = self
            .ok(&protocol::attach_backing(
                id,
                &protocol::entries(&addresses, PAGE as u32),
            ))
            .await
        {
            let _ = self.ok(&protocol::resource_unref(id)).await;
            return Err(status);
        }
        Ok(Resource {
            id,
            width,
            height,
            stride,
            size,
            mapping,
            memory,
            _pin: pin,
        })
    }

    /// Detaches the backing of `resource` and releases it, and only then
    /// closes its memory object.
    pub async fn release(&self, resource: Resource) {
        let _ = self.ok(&protocol::detach_backing(resource.id)).await;
        let _ = self.ok(&protocol::resource_unref(resource.id)).await;
        drop(resource);
    }
}
