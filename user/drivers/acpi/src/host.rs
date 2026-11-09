//! What the ACPI server gives ACPICA's operating system layer: the RSDP
//! of the `rsdp` property, and the device manager's resources, which any
//! of the layer's threads may ask for.

use mikros_acpica::Locked;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle};
use mikros_rt::interfaces::mikros::driver::{DeviceManager, DeviceManagerClient};

const FOREVER: i64 = i64::MAX;

pub struct Host {
    rsdp: u64,
    /// The device manager, one call at a time.
    manager: Locked<DeviceManagerClient>,
}

impl Host {
    pub fn new(rsdp: u64, manager: ClientEnd<DeviceManager>) -> Host {
        Host {
            rsdp,
            manager: Locked::new(DeviceManagerClient::new(manager)),
        }
    }

    pub fn manager(&self) -> mikros_acpica::Guard<'_, DeviceManagerClient> {
        self.manager.lock()
    }
}

impl mikros_acpica::Host for Host {
    fn root_pointer(&self) -> u64 {
        self.rsdp
    }

    fn request(&self, kind: u32, base: u64, size: u64, shared: bool) -> Option<OwnedHandle> {
        let granted = self
            .manager()
            .request_resource(kind, base, size, shared, FOREVER);
        match granted {
            Ok(Ok(granted)) => Some(granted.resource),
            Ok(Err(status)) => {
                mikros_rt::log!(
                    Warning,
                    "acpi: resource {kind} at {base:#x}, {size:#x} long: {status:?}"
                );
                None
            }
            Err(_) => None,
        }
    }
}

/// The address of an `rsdp` property: `0x` and lowercase hexadecimal
/// digits.
pub fn rsdp(value: &str) -> Option<u64> {
    let digits = value.strip_prefix("0x")?;
    let lowercase = digits
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if digits.is_empty() || !lowercase {
        return None;
    }
    u64::from_str_radix(digits, 16).ok()
}
