//! Against zones compiled by zic from the IANA database, with the
//! offsets that Python's zoneinfo gives for the same files.

use super::*;

fn zone(bytes: &[u8]) -> Zone {
    Zone::parse(bytes).unwrap()
}

const BERLIN: &[u8] = include_bytes!("zones/Europe_Berlin");
const NEW_YORK: &[u8] = include_bytes!("zones/America_New_York");
const SYDNEY: &[u8] = include_bytes!("zones/Australia_Sydney");
const KOLKATA: &[u8] = include_bytes!("zones/Asia_Kolkata");

/// UTC times and their offsets.
type Offsets = [(i64, i32); 9];

/// UTC times, from 1960 to 2100, and their offsets in each zone.
const OFFSETS: [(&[u8], Offsets); 4] = [
    (
        BERLIN,
        [
            (-302486400, 3600),
            (946684800, 3600),
            (1782907200, 7200),
            (1792889999, 7200),
            (1792890000, 3600),
            (2224713600, 7200),
            (2237932800, 3600),
            (4078431000, 7200),
            (4103654400, 3600),
        ],
    ),
    (
        NEW_YORK,
        [
            (-302486400, -14400),
            (946684800, -18000),
            (1782907200, -14400),
            (1792889999, -14400),
            (1792890000, -14400),
            (2224713600, -14400),
            (2237932800, -18000),
            (4078431000, -14400),
            (4103654400, -18000),
        ],
    ),
    (
        SYDNEY,
        [
            (-302486400, 36000),
            (946684800, 39600),
            (1782907200, 36000),
            (1792889999, 39600),
            (1792890000, 39600),
            (2224713600, 36000),
            (2237932800, 39600),
            (4078431000, 39600),
            (4103654400, 39600),
        ],
    ),
    (
        KOLKATA,
        [
            (-302486400, 19800),
            (946684800, 19800),
            (1782907200, 19800),
            (1792889999, 19800),
            (1792890000, 19800),
            (2224713600, 19800),
            (2237932800, 19800),
            (4078431000, 19800),
            (4103654400, 19800),
        ],
    ),
];

#[test]
fn gives_the_offset_at_a_time_before_during_and_after_the_transitions() {
    for (bytes, cases) in OFFSETS {
        let zone = zone(bytes);
        for (utc, offset) in cases {
            assert_eq!(zone.offset_at(utc), offset, "at {utc}");
        }
    }
}

#[test]
fn takes_the_earlier_offset_for_a_local_time_that_occurs_twice() {
    // 2026-10-25 02:30 in Berlin, first in CEST.
    assert_eq!(zone(BERLIN).utc_of_local(1792895400), 1792888200);
    // 2026-11-01 01:30 in New York, first in EDT.
    assert_eq!(zone(NEW_YORK).utc_of_local(1793496600), 1793511000);
    // 2026-04-05 02:30 in Sydney, first in AEDT.
    assert_eq!(zone(SYDNEY).utc_of_local(1775356200), 1775316600);
    // 2045-10-29 02:30 in Berlin, after the transitions, by the rule.
    assert_eq!(zone(BERLIN).utc_of_local(2392857000), 2392849800);
}

#[test]
fn takes_the_offset_before_a_gap_for_a_local_time_that_does_not_occur() {
    // 2026-03-29 02:30 in Berlin and 2026-03-08 02:30 in New York.
    assert_eq!(zone(BERLIN).utc_of_local(1774751400), 1774747800);
    assert_eq!(zone(NEW_YORK).utc_of_local(1772937000), 1772955000);
}

#[test]
fn converts_a_local_time_that_occurs_once() {
    // 2026-07-01 12:00 in Berlin, in CEST.
    assert_eq!(zone(BERLIN).utc_of_local(1782907200), 1782900000);
    assert_eq!(zone(KOLKATA).utc_of_local(19800), 0);
}

#[test]
fn reads_footer_rules() {
    assert_eq!(
        Rule::parse("CET-1CEST,M3.5.0,M10.5.0/3"),
        Some(Rule {
            standard: 3600,
            daylight: Some((
                7200,
                Date::Month(3, 5, 0),
                7200,
                Date::Month(10, 5, 0),
                10800
            )),
        })
    );
    assert_eq!(
        Rule::parse("IST-5:30"),
        Some(Rule {
            standard: 19800,
            daylight: None
        })
    );
    assert_eq!(
        Rule::parse("<+0330>-3:30"),
        Some(Rule {
            standard: 12600,
            daylight: None
        })
    );
    assert_eq!(Rule::parse("XY-1"), None);
    assert_eq!(Rule::parse("CET-1CEST,M13.5.0,M10.5.0"), None);
}

#[test]
fn refuses_what_is_no_tzif_file_of_version_2() {
    assert_eq!(Zone::parse(b"not a zone"), Err(Invalid));
    let mut v1 = BERLIN.to_vec();
    v1[4] = 0;
    assert_eq!(Zone::parse(&v1), Err(Invalid));
    assert_eq!(Zone::parse(&BERLIN[..BERLIN.len() / 2]), Err(Invalid));
}

#[test]
fn counts_days_as_the_calendar_does() {
    assert_eq!(days(1970, 1, 1), 0);
    assert_eq!(days(2000, 3, 1), 11017);
    assert_eq!(civil(11017), (2000, 3, 1));
    assert_eq!(Date::Julian(60).day(2024), days(2024, 3, 1));
    assert_eq!(Date::Zero(59).day(2024), days(2024, 2, 29));
    // The last Sunday of March 2026 is the 29th.
    assert_eq!(Date::Month(3, 5, 0).day(2026), days(2026, 3, 29));
}
