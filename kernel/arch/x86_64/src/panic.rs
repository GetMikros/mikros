//! Stopping the kernel on a panic.

use crate::serial::Serial;
use core::fmt::Write;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let _ = write!(Serial, "KERNEL PANIC: {}\r\n", info.message());
    let _ = write!(Serial, "END KERNEL PANIC\r\n");
    crate::cpu::halt()
}
