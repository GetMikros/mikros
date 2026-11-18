//! Host fuzz targets of user space (MIKSPEC-0017.R25): the decoding of
//! the requests of every protocol in `midl/` through the bindings that
//! midlc generates, here against the mock kernel of midl-tests, and the
//! permission broker's store of grants. And the drivers' parsers of what
//! hot-pluggable devices and media report (MIKSPEC-0017.R26,
//! MIKSPEC-0025.R67): xhci's USB descriptors, usbhid's report
//! descriptors, the partition driver's tables and probes, and FAT and
//! exFAT volumes, each from its driver's own source.

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

/// xhci's USB descriptors, from its own source.
#[allow(dead_code)]
#[path = "../../../user/drivers/xhci/src/descriptors.rs"]
pub mod usb;

/// usbhid's report descriptors, from its own source.
#[allow(dead_code)]
#[path = "../../../user/drivers/usbhid/src/hid/descriptor.rs"]
pub mod hid;

/// The partition driver's GPT, MBR and file system probes, from its own
/// source.
#[allow(dead_code)]
#[path = "../../../user/drivers/partition/src/gpt/mod.rs"]
pub mod gpt;

/// A disk image in memory for the FAT and exFAT volumes.
pub struct Image(pub Vec<u8>);

impl mikros_fat::Device for Image {
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), mikros_fat::Error> {
        let at = usize::try_from(offset).map_err(|_| mikros_fat::Error::Io)?;
        let end = at.checked_add(buf.len()).ok_or(mikros_fat::Error::Io)?;
        buf.copy_from_slice(self.0.get(at..end).ok_or(mikros_fat::Error::Io)?);
        Ok(())
    }

    fn write(&mut self, _offset: u64, _data: &[u8]) -> Result<(), mikros_fat::Error> {
        Err(mikros_fat::Error::ReadOnly)
    }

    fn flush(&mut self) -> Result<(), mikros_fat::Error> {
        Ok(())
    }
}

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
