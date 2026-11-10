//! The UTC times of FAT stamps, which hold local time, in a time zone.

use crate::Stamp;
use alloc::rc::Rc;

const NS: i64 = 1_000_000_000;
const DAY: i64 = 86_400;

/// Local time in a time zone, or in UTC.
#[derive(Clone, Debug, Default)]
pub struct Local(Option<Rc<mikros_tz::Zone>>);

impl Local {
    /// Local time that is UTC.
    pub fn utc() -> Local {
        Local(None)
    }

    /// Local time in `zone`.
    pub fn zone(zone: mikros_tz::Zone) -> Local {
        Local(Some(Rc::new(zone)))
    }

    /// The offset of local time from UTC, in seconds, at the UTC time
    /// `utc`, in nanoseconds since 1970.
    pub fn offset_at(&self, utc: i64) -> i32 {
        match &self.0 {
            Some(zone) => zone.offset_at(utc.div_euclid(NS)),
            None => 0,
        }
    }

    /// The UTC time, in nanoseconds since 1970, of the local time that
    /// `stamp` holds: by the offset that the zone has then, the earlier of
    /// two for a local time that occurs twice, and the one before a gap
    /// for one that does not occur.
    pub fn to_utc(&self, stamp: Stamp) -> i64 {
        let (year, month, day, hour, minute, second, ms) = stamp.parts();
        let date = mikros_tz::days(i64::from(year), month, day);
        let local = date * DAY + i64::from(hour * 3600 + minute * 60 + second);
        let seconds = match &self.0 {
            Some(zone) => zone.utc_of_local(local),
            None => local,
        };
        seconds * NS + i64::from(ms) * 1_000_000
    }

    /// The stamp of the local time of the UTC time `utc`, clamped to
    /// 1980–2107.
    pub fn to_stamp(&self, utc: i64) -> Stamp {
        let seconds = utc.div_euclid(NS) + i64::from(self.offset_at(utc));
        let ms = utc.rem_euclid(NS) / 1_000_000;
        let (year, month, day) = mikros_tz::civil(seconds.div_euclid(DAY));
        let time = seconds.rem_euclid(DAY);
        if year < 1980 {
            return Stamp::new(1980, 1, 1, 0, 0, 0, 0);
        }
        if year > 2107 {
            return Stamp::new(2107, 12, 31, 23, 59, 59, 990);
        }
        let (h, m, s) = (time / 3600, time / 60 % 60, time % 60);
        Stamp::new(
            year as u32,
            month,
            day,
            h as u32,
            m as u32,
            s as u32,
            ms as u32,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BERLIN: &[u8] = include_bytes!("../../tz/src/zones/Europe_Berlin");
    const KOLKATA: &[u8] = include_bytes!("../../tz/src/zones/Asia_Kolkata");

    fn local(bytes: &[u8]) -> Local {
        Local::zone(mikros_tz::Zone::parse(bytes).unwrap())
    }

    /// The UTC time of a date and time, in nanoseconds since 1970.
    fn utc(year: i64, month: u32, day: u32, hour: i64, minute: i64) -> i64 {
        (mikros_tz::days(year, month, day) * DAY + hour * 3600 + minute * 60) * NS
    }

    #[test]
    fn writes_and_reads_local_time_of_the_zone() {
        let berlin = local(BERLIN);
        // 12:00 UTC is 13:00 in winter and 14:00 in summer in Berlin.
        let winter = utc(2024, 1, 15, 12, 0);
        let stamp = berlin.to_stamp(winter);
        assert_eq!(stamp.parts(), (2024, 1, 15, 13, 0, 0, 0));
        assert_eq!(berlin.to_utc(stamp), winter);
        let summer = utc(2024, 7, 15, 12, 0) + 1_230_000_000;
        let stamp = berlin.to_stamp(summer);
        assert_eq!(stamp.parts(), (2024, 7, 15, 14, 0, 1, 230));
        assert_eq!(berlin.to_utc(stamp), summer);
        let kolkata = local(KOLKATA);
        assert_eq!(kolkata.offset_at(winter), 5 * 3600 + 30 * 60);
        assert_eq!(
            kolkata.to_stamp(winter).parts(),
            (2024, 1, 15, 17, 30, 0, 0)
        );
        // In UTC, local time is UTC.
        let stamp = Local::utc().to_stamp(winter);
        assert_eq!(stamp.parts(), (2024, 1, 15, 12, 0, 0, 0));
        assert_eq!(Local::default().to_utc(stamp), winter);
    }

    #[test]
    fn takes_the_earlier_offset_for_a_time_that_occurs_twice() {
        let berlin = local(BERLIN);
        // On 2024-10-27 clocks went from 03:00 back to 02:00; 02:30 came
        // first at 00:30 UTC.
        let twice = Stamp::new(2024, 10, 27, 2, 30, 0, 0);
        assert_eq!(berlin.to_utc(twice), utc(2024, 10, 27, 0, 30));
        // On 2024-03-31 they went from 02:00 to 03:00; 02:30 takes the
        // offset before the gap.
        let gap = Stamp::new(2024, 3, 31, 2, 30, 0, 0);
        assert_eq!(berlin.to_utc(gap), utc(2024, 3, 31, 1, 30));
    }

    #[test]
    fn clamps_to_what_fat_holds() {
        let berlin = local(BERLIN);
        assert_eq!(berlin.to_stamp(0).parts(), (1980, 1, 1, 0, 0, 0, 0));
        let late = utc(2200, 1, 1, 0, 0);
        assert_eq!(
            berlin.to_stamp(late).parts(),
            (2107, 12, 31, 23, 59, 59, 990)
        );
    }
}
