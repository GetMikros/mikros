use crate::hid::descriptor::*;

/// A boot keyboard's report descriptor: modifier bits, a reserved byte,
/// LED output bits with padding, and a six-key array.
pub(crate) const KEYBOARD: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01,
    0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x05, 0x75, 0x01,
    0x05, 0x08, 0x19, 0x01, 0x29, 0x05, 0x91, 0x02, 0x95, 0x01, 0x75, 0x03, 0x91, 0x01, 0x95, 0x06,
    0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x05, 0x07, 0x19, 0x00, 0x29, 0x65, 0x81, 0x00, 0xC0,
];

/// An absolute pointer like QEMU's tablet: three buttons, X and Y from
/// 0 to 0x7FFF, and a wheel.
pub(crate) const TABLET: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03,
    0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05, 0x81, 0x01,
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x00, 0x26, 0xFF, 0x7F, 0x35, 0x00, 0x46, 0xFF, 0x7F,
    0x75, 0x10, 0x95, 0x02, 0x81, 0x02, 0x05, 0x01, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7F, 0x35, 0x00,
    0x45, 0x00, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06, 0xC0, 0xC0,
];

#[test]
fn a_boot_keyboard() {
    let d = parse(KEYBOARD).unwrap();
    assert_eq!(d.applications, [usage(GENERIC_DESKTOP, 0x06)]);
    assert!(!d.report_ids);
    let inputs: alloc::vec::Vec<_> = d.fields.iter().filter(|f| f.kind == Kind::Input).collect();
    assert_eq!(inputs.len(), 3);
    let modifiers = inputs[0];
    assert!(modifiers.variable());
    assert_eq!(
        (modifiers.offset, modifiers.size, modifiers.count),
        (0, 1, 8)
    );
    assert_eq!(
        modifiers.variable_usage(0),
        Some(usage(KEYBOARD_PAGE, 0xE0))
    );
    assert_eq!(
        modifiers.variable_usage(7),
        Some(usage(KEYBOARD_PAGE, 0xE7))
    );
    assert!(inputs[1].constant());
    let keys = inputs[2];
    assert!(!keys.variable());
    assert_eq!((keys.offset, keys.size, keys.count), (16, 8, 6));
    assert_eq!((keys.logical_minimum, keys.logical_maximum), (0, 0x65));
    assert_eq!(keys.nth_usage(4), Some(usage(KEYBOARD_PAGE, 0x04)));
    assert_eq!(d.report_len(Kind::Input), 8);
    let leds = d.fields.iter().find(|f| f.kind == Kind::Output).unwrap();
    assert_eq!(leds.variable_usage(1), Some(usage(LED, 0x02)));
    assert_eq!(d.report_len(Kind::Output), 1);
    assert!(d.fields.iter().all(|f| f.application == Some(0)));
}

const KEYBOARD_PAGE: u16 = crate::hid::descriptor::KEYBOARD;

#[test]
fn an_absolute_pointer() {
    let d = parse(TABLET).unwrap();
    assert_eq!(d.applications, [usage(GENERIC_DESKTOP, 0x02)]);
    let xy = &d.fields[2];
    assert_eq!((xy.offset, xy.size, xy.count), (8, 16, 2));
    assert_eq!((xy.logical_minimum, xy.logical_maximum), (0, 0x7FFF));
    assert!(!xy.relative());
    assert_eq!(xy.variable_usage(1), Some(usage(GENERIC_DESKTOP, 0x31)));
    let wheel = &d.fields[3];
    assert!(wheel.relative());
    assert_eq!((wheel.logical_minimum, wheel.logical_maximum), (-127, 127));
    assert_eq!(d.report_len(Kind::Input), 6);
    // The physical collection inside keeps the application.
    assert!(d.fields.iter().all(|f| f.application == Some(0)));
}

#[test]
fn values_are_read_by_bit_and_sign_extended() {
    let d = parse(TABLET).unwrap();
    let report = [0b101, 0x34, 0x12, 0xFF, 0x7F, 0xFE];
    assert_eq!(d.fields[0].value(&report, 0), Some(1));
    assert_eq!(d.fields[0].value(&report, 1), Some(0));
    assert_eq!(d.fields[0].value(&report, 2), Some(1));
    assert_eq!(d.fields[2].value(&report, 0), Some(0x1234));
    assert_eq!(d.fields[2].value(&report, 1), Some(0x7FFF));
    assert_eq!(d.fields[3].value(&report, 0), Some(-2));
    // Past the end of a short report.
    assert_eq!(d.fields[3].value(&report[..5], 0), None);
}

#[test]
fn report_ids_offsets_and_push_pop() {
    let d = parse(&[
        0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, // Mouse application.
        0x85, 0x01, 0x05, 0x09, 0x19, 0x01, 0x29, 0x05, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95,
        0x05, 0x81, 0x02, // Report 1: five buttons...
        0xA4, // Push.
        0x75, 0x03, 0x95, 0x01, 0x81, 0x01, // ...and padding with other globals...
        0xB4, // Pop.
        0x85, 0x02, 0x81, 0x02, // Report 2 starts at bit 0 again, with 1-bit values.
        0xC0, 0x05, 0x0C, 0x0A, 0x38, 0x02, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06, // Outside.
    ])
    .unwrap();
    assert!(d.report_ids);
    let f = &d.fields;
    assert_eq!((f[0].report_id, f[0].offset, f[0].count), (1, 0, 5));
    assert_eq!((f[1].report_id, f[1].offset, f[1].size), (1, 5, 3));
    assert_eq!(
        (f[2].report_id, f[2].offset, f[2].size, f[2].count),
        (2, 0, 1, 5)
    );
    assert_eq!(f[3].application, None);
    assert_eq!(f[3].variable_usage(0), Some(usage(CONSUMER, 0x0238)));
    assert_eq!(d.report_len_of(Kind::Input, 1), Some(2));
    assert_eq!(d.report_len_of(Kind::Input, 9), None);
}

#[test]
fn four_byte_usages_carry_their_page() {
    let d = parse(&[
        0x0B, 0x38, 0x02, 0x0C, 0x00, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06,
    ])
    .unwrap();
    assert_eq!(d.fields[0].variable_usage(0), Some(usage(CONSUMER, 0x0238)));
}

#[test]
fn long_items_are_skipped() {
    let d = parse(&[
        0xFE, 0x02, 0x10, 0xAA, 0xBB, 0x75, 0x08, 0x95, 0x01, 0x81, 0x01,
    ])
    .unwrap();
    assert_eq!(d.fields.len(), 1);
}

#[test]
fn malformed_descriptors() {
    assert_eq!(parse(&[0x26, 0xFF]), Err(Error::Truncated));
    assert_eq!(parse(&[0xFE, 0x05, 0x00]), Err(Error::Truncated));
    assert_eq!(parse(&[0xB4]), Err(Error::Stack));
    assert_eq!(parse(&[0xC0]), Err(Error::Collection));
    assert_eq!(parse(&[0x85, 0x00]), Err(Error::ReportId));
    // 8 bits times 1025 values.
    assert_eq!(
        parse(&[0x75, 0x08, 0x96, 0x01, 0x04, 0x81, 0x02]),
        Err(Error::TooLong)
    );
    let mut pushes = [0xA4u8; 17].to_vec();
    pushes.push(0xB4);
    assert_eq!(parse(&pushes), Err(Error::Stack));
}

#[test]
fn degenerate_fields_do_not_overflow() {
    // A 0-bit field with a negative minimum, and a span of every usage.
    let d = parse(&[0x15, 0xFF, 0x75, 0x00, 0x95, 0x01, 0x81, 0x02]).unwrap();
    assert_eq!(d.fields[0].value(&[0xFF], 0), Some(0));
    let d = parse(&[
        0x1B, 0x00, 0x00, 0x00, 0x00, 0x2B, 0xFF, 0xFF, 0xFF, 0xFF, 0x75, 0x08, 0x95, 0x01, 0x81,
        0x00,
    ])
    .unwrap();
    assert_eq!(d.fields[0].nth_usage(u32::MAX - 1), Some(u32::MAX - 1));
}
