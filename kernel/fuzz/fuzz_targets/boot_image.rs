//! Finds the init member of a boot image.

#![no_main]

use kernel_core::ustar;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(init) = ustar::find(data, b"init") {
        // The member's data lies inside the image.
        let start = init.as_ptr() as usize - data.as_ptr() as usize;
        assert!(start + init.len() <= data.len());
        assert_eq!(start % 512, 0);
    }
});
