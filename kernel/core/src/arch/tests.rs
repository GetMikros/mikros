use super::mock;

#[test]
fn restores_interrupts_only_when_they_were_enabled() {
    let _mock = mock::lock();
    assert!(!super::interrupts_disable());
    // SAFETY: nothing here needs interrupts disabled.
    unsafe { super::interrupts_restore(true) };
    assert!(super::interrupts_enabled());
    assert!(super::interrupts_disable());
    assert!(!super::interrupts_enabled());
    // SAFETY: as above.
    unsafe { super::interrupts_restore(false) };
    assert!(!super::interrupts_enabled());
}

#[test]
fn keeps_the_cpu_data() {
    let _mock = mock::lock();
    static DATA: crate::cpu::Cpu = crate::cpu::Cpu::new();
    assert_eq!(super::cpu_index(), 0);
    assert!(super::cpu_data().is_null());
    // SAFETY: `DATA` is static and only read as a `Cpu`.
    unsafe { crate::cpu::start(&DATA) };
    assert_eq!(super::cpu_data(), (&raw const DATA).cast());
    assert!(core::ptr::eq(crate::cpu::this(), &DATA));
}

#[test]
fn uses_the_boot_cpu_until_the_data_is_set() {
    let _mock = mock::lock();
    let boot = crate::cpu::this();
    crate::cpu::start_boot();
    assert!(core::ptr::eq(crate::cpu::this(), boot));
}

#[test]
fn reads_the_clock_and_arms_the_timer() {
    let _mock = mock::lock();
    mock::set_now(1_000);
    assert_eq!(super::monotonic_now(), 1_000);
    assert_eq!(mock::timer_deadline(), None);
    super::timer_arm(5_000);
    assert_eq!(mock::timer_deadline(), Some(5_000));
    super::timer_disarm();
    assert_eq!(mock::timer_deadline(), None);
}

#[test]
fn collects_debug_output() {
    let _mock = mock::lock();
    super::debug_write(b"boot ");
    super::debug_write(b"ok");
    assert_eq!(mock::take_output(), b"boot ok");
    assert!(mock::take_output().is_empty());
}

#[test]
fn describes_the_extended_state() {
    let (size, align) = super::extended_state_layout();
    assert!(size > 0 && align.is_power_of_two());
}
