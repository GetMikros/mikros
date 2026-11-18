//! Host fuzz targets of user space (MIKSPEC-0017.R25): the decoding of
//! the requests of every protocol in `midl/` through the bindings that
//! midlc generates, here against the mock kernel of midl-tests, and the
//! permission broker's store of grants.

extern crate alloc;

pub use midl_tests::mock;

#[allow(clippy::all, dead_code, unused)]
pub mod midl {
    include!(concat!(env!("OUT_DIR"), "/midl.rs"));
}

/// The broker's store, from its own source.
#[allow(dead_code)]
#[path = "../../../user/services/permbroker/src/permissions/mod.rs"]
pub mod permissions;

use midl::__midl::{OwnedHandle, ServerEnd};

include!(concat!(env!("OUT_DIR"), "/serve.rs"));

/// Writes `bytes` with `handles` new events as one message to a new
/// channel, and decodes it as a request of the protocol `index` of
/// `PROTOCOLS`. Every handle must be closed afterwards, the message's
/// too, whether it decoded or not.
pub fn decode(index: usize, bytes: &[u8], handles: usize) {
    let before = mock::open_handles();
    let (client, server) = mock::channel_create().expect("the mock makes channels");
    let events: Vec<u32> = (0..handles).map(|_| mock::event(u32::MAX)).collect();
    let _ = mock::channel_write(client, bytes, &events);
    // SAFETY: the mock just made the handle, which nothing else holds.
    let end = unsafe { OwnedHandle::from_raw(server) };
    serve(index, end);
    mock::handle_close(client);
    assert_eq!(mock::open_handles(), before, "a request left handles open");
}
