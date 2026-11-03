//! The power buttons.

use bootmenu::menu::Power;
use bootmenu::power::{self, OS_INDICATIONS_ATTRIBUTES, Reset};
use uefi::runtime::{self, ResetType, VariableAttributes, VariableVendor};
use uefi::{Status, cstr16};

/// Whether the firmware can be asked to show its setup screen.
pub fn firmware_ui_supported() -> bool {
    let mut buf = [0u8; 8];
    let name = cstr16!("OsIndicationsSupported");
    let value = runtime::get_variable(name, &VariableVendor::GLOBAL_VARIABLE, &mut buf);
    power::firmware_ui_supported(value.ok().map(|(value, _)| &*value))
}

/// Carries out `power`. Does not return.
pub fn act(power: Power) -> ! {
    let name = cstr16!("OsIndications");
    let vendor = VariableVendor::GLOBAL_VARIABLE;
    let plan = power::plan(power, || {
        let mut buf = [0u8; 8];
        let (value, _) = runtime::get_variable(name, &vendor, &mut buf).ok()?;
        power::u64_value(value)
    });
    if let Some(value) = plan.os_indications {
        let attributes = VariableAttributes::from_bits_retain(OS_INDICATIONS_ATTRIBUTES);
        let _ = runtime::set_variable(name, &vendor, attributes, &value.to_le_bytes());
    }
    let reset = match plan.reset {
        Reset::Cold => ResetType::COLD,
        Reset::Shutdown => ResetType::SHUTDOWN,
    };
    runtime::reset(reset, Status::SUCCESS, None)
}
