//! usbhid's parser of HID report descriptors (MIKSPEC-0017.R26), fed the
//! same bytes as a hostile device could send them.

#![no_main]

use libfuzzer_sys::fuzz_target;
use user_fuzz::hid::parse;

fuzz_target!(|data: &[u8]| {
    if let Ok(descriptor) = parse(data) {
        for field in &descriptor.fields {
            for n in [0, 1, 7, u32::MAX] {
                let _ = field.nth_usage(n);
            }
        }
    }
});
