//! The real-time clock, through the first instance of `device.rtc` that
//! is published: the time service sets UTC from it while `SetUtc` has not
//! set the time since boot, and writes it when the time is set, as UTC
//! or, when `etc/time.yaml` says so, as local time.

use crate::clock::Time;
use alloc::rc::Rc;
use alloc::string::String;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::interfaces::mikros::time::{Rtc, RtcAsyncClient};

/// The service of real-time clock drivers.
pub const RTC: &str = "device.rtc";

/// Connects to the real-time clock `instance` through `directory`, and
/// gives it to `time`.
pub async fn connected(
    time: Rc<Time>,
    directory: Rc<DirectoryAsyncClient>,
    instance: String,
    reactor: Reactor,
) {
    let Ok((client, server)) = endpoints::<Rtc>() else {
        return;
    };
    let connected = directory
        .connect(RTC.into(), Some(instance), server.into_handle())
        .await;
    if !matches!(connected, Ok(Ok(_))) {
        mikros_rt::log!(Error, "time: connecting to the real-time clock failed");
        return;
    }
    time.rtc_available(RtcAsyncClient::new(client, reactor))
        .await;
}
