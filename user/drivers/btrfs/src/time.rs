// SPDX-License-Identifier: GPL-2.0
//! The time page of the time service, which gives UTC as an offset from
//! the monotonic clock.

use mikros_rt::interfaces::__midl::{ClientEnd, endpoints};
use mikros_rt::interfaces::mikros::service::DirectoryClient;
use mikros_rt::interfaces::mikros::time::{Clock, ClockClient};
use mikros_rt::sys;

const CLOCK_PROTOCOL: &str = "mikros.time.Clock";
const FOREVER: i64 = i64::MAX;
const PERM_READ: u32 = 1 << 0;

/// Maps the time page of the time service in `directory`, when it runs.
pub fn page(directory: &DirectoryClient, process: sys::Handle) -> Option<u64> {
    let (client, server) = endpoints::<Clock>().ok()?;
    match directory.connect(CLOCK_PROTOCOL.into(), None, server.into_handle(), FOREVER) {
        Ok(Ok(_)) => {}
        _ => return None,
    }
    let clock = ClockClient::new(ClientEnd::new(client.into_handle()));
    let page = clock.get_time_page(FOREVER).ok()?.page;
    let mut at = 0;
    // SAFETY: the out pointer is valid; the kernel picks a free range.
    let status = unsafe { sys::vm_map(process, PERM_READ, 0, page.raw(), 0, 4096, &mut at) };
    (status == mikros_rt::OK).then_some(at)
}
