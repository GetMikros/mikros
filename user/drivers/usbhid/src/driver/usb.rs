//! The USB side of HID: the class requests, the HID descriptor in the
//! configuration descriptor, and keeping input transfers outstanding.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::future::{Future, poll_fn};
use core::pin::Pin;
use core::task::Poll;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::usb::{InterfaceAsyncClient, Setup};
use mikros_rt::sys;

/// Requests to the interface: standard GET_DESCRIPTOR, and the class
/// requests SET_REPORT, SET_IDLE and SET_PROTOCOL.
const DEVICE_TO_HOST_INTERFACE: u8 = 0x81;
const HOST_TO_DEVICE_CLASS_INTERFACE: u8 = 0x21;
const GET_DESCRIPTOR: u8 = 0x06;
const SET_REPORT: u8 = 0x09;
const SET_IDLE: u8 = 0x0A;
const SET_PROTOCOL: u8 = 0x0B;
/// Descriptor types.
const INTERFACE: u8 = 0x04;
const HID: u8 = 0x21;
const REPORT: u8 = 0x22;
/// SET_PROTOCOL's value for the report protocol, and SET_REPORT's
/// report type for output reports.
const REPORT_PROTOCOL: u16 = 1;
const OUTPUT_REPORT: u16 = 2;
/// How long a control request may take.
const CONTROL_LIMIT: i64 = 1_000_000_000;

/// The monotonic clock, in nanoseconds.
pub fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

/// The report descriptor length that the HID descriptor of interface
/// `number`, alternate setting 0, gives in `configuration`.
pub fn report_descriptor_len(configuration: &[u8], number: u8) -> Option<u16> {
    let mut at = 0;
    let mut ours = false;
    while at + 2 <= configuration.len() {
        let len = usize::from(configuration[at]);
        if len < 2 || at + len > configuration.len() {
            return None;
        }
        let d = &configuration[at..at + len];
        match d[1] {
            INTERFACE if len >= 4 => ours = d[2] == number && d[3] == 0,
            HID if ours && len >= 6 => {
                // Pairs of type and length follow the count at 5.
                let count = usize::from(d[5]);
                for pair in d[6..].as_chunks::<3>().0.iter().take(count) {
                    if pair[0] == REPORT {
                        return Some(u16::from_le_bytes([pair[1], pair[2]]));
                    }
                }
                return None;
            }
            _ => {}
        }
        at += len;
    }
    None
}

async fn control(
    bus: &InterfaceAsyncClient,
    setup: Setup,
    data: Vec<u8>,
) -> Result<Vec<u8>, Status> {
    match bus
        .control_transfer(setup, data, now() + CONTROL_LIMIT)
        .await
    {
        Ok(Ok(reply)) => Ok(reply.data),
        Ok(Err(status)) => Err(status),
        Err(_) => Err(Status::PeerClosed),
    }
}

pub async fn report_descriptor(
    bus: &InterfaceAsyncClient,
    number: u8,
    len: u16,
) -> Result<Vec<u8>, Status> {
    let setup = Setup {
        request_type: DEVICE_TO_HOST_INTERFACE,
        request: GET_DESCRIPTOR,
        value: u16::from(REPORT) << 8,
        index: u16::from(number),
        length: len,
    };
    control(bus, setup, Vec::new()).await
}

fn class_request(request: u8, value: u16, number: u8, length: u16) -> Setup {
    Setup {
        request_type: HOST_TO_DEVICE_CLASS_INTERFACE,
        request,
        value,
        index: u16::from(number),
        length,
    }
}

pub async fn set_report_protocol(bus: &InterfaceAsyncClient, number: u8) -> Result<(), Status> {
    let setup = class_request(SET_PROTOCOL, REPORT_PROTOCOL, number, 0);
    control(bus, setup, Vec::new()).await.map(|_| ())
}

/// SET_IDLE with duration 0, so that reports come only on changes.
pub async fn set_idle(bus: &InterfaceAsyncClient, number: u8) -> Result<(), Status> {
    control(bus, class_request(SET_IDLE, 0, number, 0), Vec::new())
        .await
        .map(|_| ())
}

/// Sends output report `report`, which starts with its ID when `id` is
/// not 0, with SET_REPORT.
pub async fn set_report(
    bus: &InterfaceAsyncClient,
    number: u8,
    id: u8,
    report: Vec<u8>,
) -> Result<(), Status> {
    let length = u16::try_from(report.len()).map_err(|_| Status::InvalidArgs)?;
    let setup = class_request(
        SET_REPORT,
        OUTPUT_REPORT << 8 | u16::from(id),
        number,
        length,
    );
    control(bus, setup, report).await.map(|_| ())
}

/// Sends `report` on the interrupt OUT endpoint `endpoint`.
pub async fn interrupt_out(
    bus: &InterfaceAsyncClient,
    endpoint: u8,
    report: Vec<u8>,
) -> Result<(), Status> {
    match bus
        .interrupt_transfer(endpoint, report, 0, now() + CONTROL_LIMIT)
        .await
    {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(status)) => Err(status),
        Err(_) => Err(Status::PeerClosed),
    }
}

type Transfer = Pin<Box<dyn Future<Output = Result<Vec<u8>, Status>>>>;

/// One interrupt IN transfer, waiting as long as it takes.
fn transfer(bus: Rc<InterfaceAsyncClient>, endpoint: u8, length: u32) -> Transfer {
    Box::pin(async move {
        match bus
            .interrupt_transfer(endpoint, Vec::new(), length, i64::MAX)
            .await
        {
            Ok(Ok(reply)) => Ok(reply.data),
            Ok(Err(status)) => Err(status),
            Err(_) => Err(Status::PeerClosed),
        }
    })
}

/// Reads input reports of `length` bytes from `endpoint` and gives each
/// to `report`, in order, until a transfer fails. Two transfers are
/// queued at all times, so that one is outstanding while the other's
/// report is handled.
pub async fn poll(
    bus: Rc<InterfaceAsyncClient>,
    endpoint: u8,
    length: u32,
    mut report: impl FnMut(Vec<u8>),
) -> Status {
    let mut front = transfer(bus.clone(), endpoint, length);
    let mut back = transfer(bus.clone(), endpoint, length);
    let mut back_done: Option<Result<Vec<u8>, Status>> = None;
    loop {
        let done = poll_fn(|cx| {
            // Polling the second sends its request too, and keeps a
            // result that comes first until the first's is taken.
            if back_done.is_none()
                && let Poll::Ready(r) = back.as_mut().poll(cx)
            {
                back_done = Some(r);
            }
            front.as_mut().poll(cx)
        })
        .await;
        front = core::mem::replace(&mut back, transfer(bus.clone(), endpoint, length));
        if let Some(r) = back_done.take() {
            // The new front already completed: keep its result.
            front = Box::pin(async move { r });
        }
        match done {
            Ok(data) => report(data),
            Err(status) => return status,
        }
    }
}
