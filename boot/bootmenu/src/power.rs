//! What the power buttons do, in values that the UEFI code passes to the
//! firmware.

use crate::menu::Power;

/// Bit of `OsIndicationsSupported` and `OsIndications` that asks the
/// firmware to show its setup screen on the next boot.
pub const BOOT_TO_FW_UI: u64 = 1;

/// The attributes `OsIndications` is written with: non-volatile (1), boot
/// service access (2) and runtime access (4).
pub const OS_INDICATIONS_ATTRIBUTES: u32 = 0x1 | 0x2 | 0x4;

/// The kind of reset a button ends with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reset {
    Cold,
    Shutdown,
}

/// What a power button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The value to write to `OsIndications`, with
    /// `OS_INDICATIONS_ATTRIBUTES`, before the reset.
    pub os_indications: Option<u64>,
    pub reset: Reset,
}

/// Reads a 64-bit variable. Returns `None` unless `data` is 8 bytes long.
pub fn u64_value(data: &[u8]) -> Option<u64> {
    Some(u64::from_le_bytes(data.try_into().ok()?))
}

/// Whether the firmware can be asked to show its setup screen, given the
/// contents of `OsIndicationsSupported` if it could be read.
pub fn firmware_ui_supported(os_indications_supported: Option<&[u8]>) -> bool {
    os_indications_supported
        .and_then(u64_value)
        .is_some_and(|value| value & BOOT_TO_FW_UI != 0)
}

/// What `power` does. `os_indications` reads the current contents of
/// `OsIndications`, and is called only for the firmware settings button.
pub fn plan(power: Power, os_indications: impl FnOnce() -> Option<u64>) -> Plan {
    match power {
        Power::Restart => Plan {
            os_indications: None,
            reset: Reset::Cold,
        },
        Power::ShutDown => Plan {
            os_indications: None,
            reset: Reset::Shutdown,
        },
        Power::FirmwareSettings => Plan {
            os_indications: Some(os_indications().unwrap_or(0) | BOOT_TO_FW_UI),
            reset: Reset::Cold,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restarts_and_shuts_down_without_writing_variables() {
        let unread = || -> Option<u64> { panic!("OsIndications read") };
        assert_eq!(
            plan(Power::Restart, unread),
            Plan {
                os_indications: None,
                reset: Reset::Cold
            }
        );
        assert_eq!(
            plan(Power::ShutDown, unread),
            Plan {
                os_indications: None,
                reset: Reset::Shutdown
            }
        );
    }

    #[test]
    fn firmware_settings_set_the_bit_and_keep_the_others() {
        let plan = plan(Power::FirmwareSettings, || Some(0x8000_0000_0000_0014));
        assert_eq!(plan.os_indications, Some(0x8000_0000_0000_0015));
        assert_eq!(plan.reset, Reset::Cold);
        let unreadable = super::plan(Power::FirmwareSettings, || None);
        assert_eq!(unreadable.os_indications, Some(BOOT_TO_FW_UI));
        assert_eq!(unreadable.reset, Reset::Cold);
        assert_eq!(OS_INDICATIONS_ATTRIBUTES, 7);
    }

    #[test]
    fn offers_firmware_settings_only_when_supported() {
        let value = |v: u64| v.to_le_bytes();
        assert!(firmware_ui_supported(Some(&value(1))));
        assert!(firmware_ui_supported(Some(&value(0x41))));
        assert!(!firmware_ui_supported(Some(&value(0x40))));
        assert!(!firmware_ui_supported(Some(&[1, 0, 0, 0])), "too short");
        assert!(!firmware_ui_supported(None), "variable missing");
    }
}
