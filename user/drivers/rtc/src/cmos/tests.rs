use super::*;

const BCD_24: Format = Format {
    binary: false,
    hours_24: true,
};
const BINARY_12: Format = Format {
    binary: true,
    hours_24: false,
};
/// 2026-11-09T13:45:30, a Monday.
const MONDAY: i64 = 1_794_231_930;

fn bcd(seconds: u8, minutes: u8, hours: u8, day: u8, month: u8, year: u8) -> Registers {
    Registers {
        seconds,
        minutes,
        hours,
        weekday: 0,
        day,
        month,
        year,
        century: None,
    }
}

#[test]
fn reads_the_format_from_status_register_b() {
    assert_eq!(Format::of(0x02), BCD_24);
    assert_eq!(Format::of(0x04), BINARY_12);
    assert!(Format::of(0x86).binary);
}

#[test]
fn decodes_binary_coded_decimal_in_the_years_2000_to_2099() {
    let r = bcd(0x30, 0x45, 0x13, 0x09, 0x11, 0x26);
    assert_eq!(decode(&r, BCD_24), Some(MONDAY));
    let r = bcd(0x00, 0x00, 0x00, 0x01, 0x01, 0x00);
    assert_eq!(decode(&r, BCD_24), Some(946_684_800));
}

#[test]
fn decodes_12_hour_values() {
    let midnight = Registers {
        hours: 12,
        ..bcd(0, 0, 0, 1, 1, 0)
    };
    assert_eq!(decode(&midnight, BINARY_12), Some(946_684_800));
    let noon = Registers {
        hours: 12 | PM,
        ..midnight
    };
    assert_eq!(decode(&noon, BINARY_12), Some(946_684_800 + 12 * 3600));
    let one_pm = Registers {
        hours: 1 | PM,
        ..midnight
    };
    assert_eq!(decode(&one_pm, BINARY_12), Some(946_684_800 + 13 * 3600));
    let zero = Registers {
        hours: 0,
        ..midnight
    };
    assert_eq!(decode(&zero, BINARY_12), None);
}

#[test]
fn takes_the_century_register_when_there_is_one() {
    let r = Registers {
        century: Some(0x19),
        ..bcd(0x00, 0x00, 0x00, 0x01, 0x01, 0x70)
    };
    assert_eq!(decode(&r, BCD_24), Some(0));
}

#[test]
fn refuses_values_that_are_no_date_and_time() {
    // A digit above 9, month 0, 31 November, 29 February outside a leap
    // year, and hour 24.
    for r in [
        bcd(0x3A, 0x45, 0x13, 0x09, 0x11, 0x26),
        bcd(0x30, 0x45, 0x13, 0x09, 0x00, 0x26),
        bcd(0x30, 0x45, 0x13, 0x31, 0x11, 0x26),
        bcd(0x00, 0x00, 0x00, 0x29, 0x02, 0x26),
        bcd(0x00, 0x00, 0x24, 0x01, 0x01, 0x26),
    ] {
        assert_eq!(decode(&r, BCD_24), None, "{r:?}");
    }
    assert!(decode(&bcd(0x00, 0x00, 0x00, 0x29, 0x02, 0x28), BCD_24).is_some());
}

#[test]
fn encodes_in_the_clocks_format_with_the_day_of_the_week() {
    let r = encode(MONDAY, BCD_24, false).unwrap();
    assert_eq!(
        r,
        Registers {
            weekday: 2,
            ..bcd(0x30, 0x45, 0x13, 0x09, 0x11, 0x26)
        }
    );
    let r = encode(MONDAY, BINARY_12, true).unwrap();
    assert_eq!((r.hours, r.year, r.century), (1 | PM, 26, Some(20)));
    // 2000-01-01 was a Saturday.
    assert_eq!(encode(946_684_800, BCD_24, false).unwrap().weekday, 7);
}

#[test]
fn encodes_what_it_decodes() {
    for format in [BCD_24, BINARY_12, Format::of(0x06), Format::of(0x00)] {
        for seconds in [946_684_800, MONDAY, 4_102_444_799] {
            let r = encode(seconds, format, false).unwrap();
            assert_eq!(decode(&r, format), Some(seconds));
        }
    }
}

#[test]
fn refuses_times_the_clock_cannot_hold() {
    // 1999-12-31T23:59:59 and 2100-01-01 without a century register,
    // and 10000-01-01 with one.
    assert_eq!(encode(946_684_799, BCD_24, false), None);
    assert_eq!(encode(4_102_444_800, BCD_24, false), None);
    assert!(encode(946_684_799, BCD_24, true).is_some());
    assert_eq!(encode(253_402_300_800, BCD_24, true), None);
}
