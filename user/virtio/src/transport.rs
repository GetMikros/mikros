//! Initializing a device, and taking its interrupts.

use crate::caps::{self, Structures};
use crate::queue::Queue;
use crate::{Error, F_ACCESS_PLATFORM, F_RING_PACKED, F_VERSION_1, SharedMemory};
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::Cell;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::pci::DeviceAsyncClient;
use mikros_rt::sys;

/// Fields of the common configuration structure.
const DEVICE_FEATURE_SELECT: u64 = 0x00;
const DEVICE_FEATURE: u64 = 0x04;
const DRIVER_FEATURE_SELECT: u64 = 0x08;
const DRIVER_FEATURE: u64 = 0x0C;
const CONFIG_MSIX_VECTOR: u64 = 0x10;
const DEVICE_STATUS: u64 = 0x14;
const CONFIG_GENERATION: u64 = 0x15;
const QUEUE_SELECT: u64 = 0x16;
const QUEUE_SIZE: u64 = 0x18;
const QUEUE_MSIX_VECTOR: u64 = 0x1A;
const QUEUE_ENABLE: u64 = 0x1C;
const QUEUE_NOTIFY_OFF: u64 = 0x1E;
const QUEUE_DESC: u64 = 0x20;
const QUEUE_DRIVER: u64 = 0x28;
const QUEUE_DEVICE: u64 = 0x30;
/// Device status bits.
const ACKNOWLEDGE: u8 = 1;
const DRIVER: u8 = 2;
const DRIVER_OK: u8 = 4;
const FEATURES_OK: u8 = 8;
const FAILED: u8 = 128;
/// The ISR status bits of a used ring update and a configuration change.
const ISR_QUEUE: u8 = 1 << 0;
const ISR_CONFIG: u8 = 1 << 1;
/// The largest queue the library sets up.
const QUEUE_MAX: u16 = 256;
const SIGNALED: u32 = 1 << 3;
const MS: i64 = 1_000_000;
/// How long a reset may take.
const RESET_LIMIT: i64 = 1000 * MS;

/// A device that the library initialized: the bus channel, its mapped
/// structures, its virtqueues, the features accepted, its bus
/// transaction initiator, its interrupts, and how many configuration
/// changes it reported.
pub struct Transport {
    device: Rc<DeviceAsyncClient>,
    bti: OwnedHandle,
    structures: Rc<Structures>,
    queues: Vec<Rc<Queue>>,
    features: u64,
    _interrupts: Vec<OwnedHandle>,
    config_changes: Rc<Cell<u64>>,
}

/// What `Transport::start` gives a driver.
pub struct Started {
    pub transport: Rc<Transport>,
    /// The features that both the device offered and the driver asked
    /// for, `VIRTIO_F_VERSION_1` among them.
    pub features: u64,
    pub queues: Vec<Rc<Queue>>,
}

impl Transport {
    /// Initializes the device behind the bus channel `device` in order:
    /// resets it, sets `ACKNOWLEDGE` and `DRIVER`, accepts the features
    /// that it offers and `wanted` holds, with `VIRTIO_F_VERSION_1`,
    /// which it must offer, and never `VIRTIO_F_ACCESS_PLATFORM` or
    /// `VIRTIO_F_RING_PACKED`, sets `FEATURES_OK` and checks that the
    /// device kept it, sets up `queues` virtqueues in memory made from
    /// the `DMA` resource `dma`, and sets `DRIVER_OK`. A step that fails
    /// after the reset sets `FAILED`.
    ///
    /// Interrupts come through `EnableMsi`, one for configuration changes
    /// and one per queue, the last one shared by the remaining queues
    /// when there are fewer, or through INTx when `EnableMsi` fails.
    pub async fn start(
        device: DeviceAsyncClient,
        process: sys::Handle,
        dma: &OwnedHandle,
        wanted: u64,
        queues: u16,
        reactor: &Reactor,
        spawner: &Spawner,
    ) -> Result<Started, Error> {
        let device = Rc::new(device);
        let structures = Rc::new(caps::find(&device, process).await?);
        bus(device.set_bus_master(true).await)?;
        let bti = match device.get_bti().await {
            Ok(Ok(got)) => got.bti,
            Ok(Err(status)) => return Err(crate::Error::Bus(status)),
            Err(_) => return Err(Error::Channel),
        };
        let common = &structures.common;
        reset(&structures, reactor).await?;
        let status = |bits: u8| common.write8(DEVICE_STATUS, common.read8(DEVICE_STATUS) | bits);
        status(ACKNOWLEDGE);
        status(DRIVER);
        let started = async {
            let features = negotiate(&structures, wanted)?;
            status(FEATURES_OK);
            if common.read8(DEVICE_STATUS) & FEATURES_OK == 0 {
                return Err(Error::FeaturesRefused);
            }
            let interrupts = match device.enable_msi(u32::from(queues) + 1).await {
                Ok(Ok(enabled)) => Interrupts::Msi(enabled.interrupts),
                _ => match device.get_interrupt().await {
                    Ok(Ok(got)) => Interrupts::Intx(got.event),
                    Ok(Err(status)) => return Err(Error::Bus(status)),
                    Err(_) => return Err(Error::Channel),
                },
            };
            let vectors = match &interrupts {
                Interrupts::Msi(list) => list.len() as u16,
                Interrupts::Intx(_) => 0,
            };
            if vectors > 0 {
                common.write16(CONFIG_MSIX_VECTOR, 0);
            }
            let mut made = Vec::new();
            for index in 0..queues {
                common.write16(QUEUE_SELECT, index);
                let max = common.read16(QUEUE_SIZE);
                if max == 0 {
                    return Err(Error::NoQueue(index));
                }
                let limit = max.min(QUEUE_MAX);
                let size = 1u16 << (15 - limit.leading_zeros());
                common.write16(QUEUE_SIZE, size);
                let (mut queue, [desc, driver, used]) =
                    Queue::new(index, size, dma, &bti, process)?;
                common.write64(QUEUE_DESC, desc);
                common.write64(QUEUE_DRIVER, driver);
                common.write64(QUEUE_DEVICE, used);
                if vectors > 0 {
                    common.write16(QUEUE_MSIX_VECTOR, (index + 1).min(vectors - 1));
                }
                let notify = u64::from(common.read16(QUEUE_NOTIFY_OFF))
                    * u64::from(structures.notify_multiplier);
                queue.set_notify(structures.clone(), notify);
                common.write16(QUEUE_ENABLE, 1);
                made.push(Rc::new(queue));
            }
            status(DRIVER_OK);
            Ok((features, interrupts, made))
        }
        .await;
        let (features, interrupts, made) = match started {
            Ok(started) => started,
            Err(e) => {
                status(FAILED);
                return Err(e);
            }
        };
        let config_changes = Rc::new(Cell::new(0));
        let kept = watch(
            &device,
            &structures,
            interrupts,
            &made,
            &config_changes,
            reactor,
            spawner,
        );
        let transport = Rc::new(Transport {
            device,
            bti,
            structures,
            queues: made.clone(),
            features,
            _interrupts: kept,
            config_changes,
        });
        Ok(Started {
            transport,
            features,
            queues: made,
        })
    }

    /// The device's bus transaction initiator, through which the driver
    /// pins the memory of its buffers.
    pub fn bti(&self) -> &OwnedHandle {
        &self.bti
    }

    pub fn features(&self) -> u64 {
        self.features
    }

    pub fn queues(&self) -> &[Rc<Queue>] {
        &self.queues
    }

    /// The shared memory regions the device describes.
    pub fn shared_memory(&self) -> &[SharedMemory] {
        &self.structures.shared_memory
    }

    /// How many configuration changes the device has reported.
    pub fn config_changes(&self) -> u64 {
        self.config_changes.get()
    }

    /// Reads the device-specific configuration with `read`, again until
    /// the configuration generation is the same before and after.
    pub fn read_config<T>(&self, read: impl Fn(&crate::mmio::Mmio) -> T) -> T {
        let common = &self.structures.common;
        loop {
            let before = common.read8(CONFIG_GENERATION);
            let value = read(&self.structures.device);
            if common.read8(CONFIG_GENERATION) == before {
                return value;
            }
        }
    }

    /// The bus channel, for requests of the driver's own.
    pub fn device(&self) -> &DeviceAsyncClient {
        &self.device
    }

    /// Resets the device, before its queue memory is released.
    pub async fn stop(&self, reactor: &Reactor) {
        let _ = reset(&self.structures, reactor).await;
    }
}

/// How the device's interrupts arrive.
enum Interrupts {
    Msi(Vec<OwnedHandle>),
    Intx(OwnedHandle),
}

fn bus<T>(
    reply: Result<
        Result<T, mikros_rt::interfaces::mikros::base::Status>,
        mikros_rt::interfaces::__midl::Error,
    >,
) -> Result<T, Error> {
    match reply {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(status)) => Err(Error::Bus(status)),
        Err(_) => Err(Error::Channel),
    }
}

/// Resets the device and waits until it reads back a status of 0.
async fn reset(structures: &Structures, reactor: &Reactor) -> Result<(), Error> {
    let common = &structures.common;
    common.write8(DEVICE_STATUS, 0);
    let deadline = now() + RESET_LIMIT;
    while common.read8(DEVICE_STATUS) != 0 {
        if now() >= deadline {
            return Err(Error::ResetTimedOut);
        }
        let _ = reactor.sleep_until(now() + MS).await;
    }
    Ok(())
}

/// Accepts the features that the device offers and `wanted` holds,
/// with `VIRTIO_F_VERSION_1` and without those the library never takes.
fn negotiate(structures: &Structures, wanted: u64) -> Result<u64, Error> {
    let common = &structures.common;
    let mut offered = 0u64;
    for half in 0..2u32 {
        common.write32(DEVICE_FEATURE_SELECT, half);
        offered |= u64::from(common.read32(DEVICE_FEATURE)) << (32 * half);
    }
    if offered & F_VERSION_1 == 0 {
        return Err(Error::NotModern);
    }
    let accepted = offered & (wanted | F_VERSION_1) & !(F_ACCESS_PLATFORM | F_RING_PACKED);
    for half in 0..2u32 {
        common.write32(DRIVER_FEATURE_SELECT, half);
        common.write32(DRIVER_FEATURE, (accepted >> (32 * half)) as u32);
    }
    Ok(accepted)
}

/// Takes the device's interrupts: on each one, processes the used rings
/// of the queues it belongs to and counts a configuration change, and
/// then acknowledges it. Returns the handles to keep. An MSI that arrives
/// while its object is still signaled adds no signal, so after the
/// acknowledgement the used rings are processed once more, for requests
/// that the device returned in between.
fn watch(
    device: &Rc<DeviceAsyncClient>,
    structures: &Rc<Structures>,
    interrupts: Interrupts,
    queues: &[Rc<Queue>],
    config_changes: &Rc<Cell<u64>>,
    reactor: &Reactor,
    spawner: &Spawner,
) -> Vec<OwnedHandle> {
    match interrupts {
        Interrupts::Msi(list) => {
            let vectors = list.len();
            for (vector, interrupt) in list.iter().enumerate() {
                let mine: Vec<Rc<Queue>> = queues
                    .iter()
                    .filter(|q| usize::from(q.index() + 1).min(vectors - 1) == vector)
                    .cloned()
                    .collect();
                let config = (vector == 0).then(|| config_changes.clone());
                let raw = interrupt.raw();
                let reactor = reactor.clone();
                spawner.spawn(async move {
                    while reactor.signals(raw, SIGNALED).await.is_ok() {
                        for queue in &mine {
                            queue.process_used();
                        }
                        if let Some(changes) = &config {
                            changes.set(changes.get() + 1);
                        }
                        // SAFETY: the call takes no pointers.
                        unsafe { sys::interrupt_ack(raw) };
                        for queue in &mine {
                            queue.process_used();
                        }
                    }
                });
            }
            list
        }
        Interrupts::Intx(event) => {
            let (device, structures) = (device.clone(), structures.clone());
            let (queues, changes) = (queues.to_vec(), config_changes.clone());
            let raw = event.raw();
            let reactor = reactor.clone();
            spawner.spawn(async move {
                while reactor.signals(raw, SIGNALED).await.is_ok() {
                    let isr = structures.isr.read8(0);
                    if isr & ISR_QUEUE != 0 {
                        for queue in &queues {
                            queue.process_used();
                        }
                    }
                    if isr & ISR_CONFIG != 0 {
                        changes.set(changes.get() + 1);
                    }
                    if device.ack_interrupt().await.is_err() {
                        return;
                    }
                }
            });
            alloc::vec![event]
        }
    }
}

fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}
