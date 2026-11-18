//! xhci's parsers of what a USB device reports (MIKSPEC-0017.R26): the
//! device, configuration, string and hub descriptors, fed the same bytes
//! as a hostile device could send them.

#![no_main]

use libfuzzer_sys::fuzz_target;
use user_fuzz::usb::{Configuration, Device, hub, languages, string};

fuzz_target!(|data: &[u8]| {
    let _ = Device::parse(data);
    let _ = hub(data);
    let _ = string(data);
    let _ = languages(data);
    if let Some(configuration) = Configuration::parse(data) {
        for interface in &configuration.interfaces {
            let found = configuration.interface(interface.number, interface.alternate);
            assert!(found.is_some(), "a parsed interface is found again");
            for endpoint in &interface.endpoints {
                let _ = (endpoint.id(), endpoint.kind(), endpoint.is_in());
            }
        }
    }
});
