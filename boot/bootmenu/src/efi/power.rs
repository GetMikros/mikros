//! The power buttons.

use bootmenu::menu::Power;
use uefi::runtime::{self, ResetType, VariableAttributes, VariableVendor};
use uefi::{Status, cstr16};

use super::BOOT_TO_FW_UI;

/// Carries out `power`. Does not return.
pub fn act(power: Power) -> ! {
    match power {
        Power::Restart => runtime::reset(ResetType::COLD, Status::SUCCESS, None),
        Power::ShutDown => runtime::reset(ResetType::SHUTDOWN, Status::SUCCESS, None),
        Power::FirmwareSettings => {
            let name = cstr16!("OsIndications");
            let vendor = VariableVendor::GLOBAL_VARIABLE;
            let mut buf = [0u8; 8];
            let current = match runtime::get_variable(name, &vendor, &mut buf) {
                Ok((value, _)) if value.len() == 8 => u64::from_le_bytes(buf),
                _ => 0,
            };
            let attributes = VariableAttributes::NON_VOLATILE
                | VariableAttributes::BOOTSERVICE_ACCESS
                | VariableAttributes::RUNTIME_ACCESS;
            let value = (current | BOOT_TO_FW_UI).to_le_bytes();
            let _ = runtime::set_variable(name, &vendor, attributes, &value);
            runtime::reset(ResetType::COLD, Status::SUCCESS, None)
        }
    }
}
