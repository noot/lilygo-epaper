use t5s3_epaper_core::Clock;

pub(crate) const DAY_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
pub(crate) const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

// the RTC holds UTC; below this (~year 2020) it is just counting up from boot,
// unsynced. apply the timezone offset to a synced UTC second count, or None if
// it has not been synced to a real wall-clock time yet.
fn local_secs(clock: &mut Clock, offset_hours: i8) -> Option<u64> {
    let secs = clock.now_us() / 1_000_000;
    if secs > 1_600_000_000 {
        Some((secs as i64 + i64::from(offset_hours) * 3600).max(0) as u64)
    } else {
        None
    }
}

// return (hours, minutes) of local time, or None before the first NTP sync.
pub(crate) fn status_time(clock: &mut Clock, offset_hours: i8) -> Option<(u32, u32)> {
    let local = local_secs(clock, offset_hours)?;
    let sod = (local % 86_400) as u32;
    Some((sod / 3600, (sod % 3600) / 60))
}

// return (hours, minutes, seconds) of local time, or None before the first NTP
// sync.
pub(crate) fn status_time_secs(clock: &mut Clock, offset_hours: i8) -> Option<(u32, u32, u32)> {
    let local = local_secs(clock, offset_hours)?;
    let sod = (local % 86_400) as u32;
    Some((sod / 3600, (sod % 3600) / 60, sod % 60))
}

// return (day-of-week, year, month, day) of local time, or None before the
// first NTP sync.
pub(crate) fn status_date(clock: &mut Clock, offset_hours: i8) -> Option<(usize, i64, u32, u32)> {
    let local = local_secs(clock, offset_hours)?;
    let days = (local / 86_400) as i64;
    let (year, month, day) = civil_from_days(days);
    let dow = ((days + 4) % 7) as usize; // 1970-01-01 was a Thursday; 0 = Sunday
    Some((dow, year, month, day))
}

// weekday index (0 = Sunday) for a gregorian date. used to label forecast days
// parsed from ISO date strings, without depending on the RTC being synced.
#[cfg(feature = "gps")]
pub(crate) fn weekday(year: i64, month: u32, day: u32) -> usize {
    let days = days_from_civil(year, month, day);
    (((days + 4) % 7 + 7) % 7) as usize // 1970-01-01 was a Thursday; 0 = Sunday
}

// days since the unix epoch for a gregorian (year, month, day). inverse of
// `civil_from_days`.
// see http://howardhinnant.github.io/date_algorithms.html#days_from_civil
#[cfg(feature = "gps")]
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if month > 2 { month - 3 } else { month + 9 }; // [0, 11]
    let doy = (153 * i64::from(mp) + 2) / 5 + i64::from(day) - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

// gregorian (year, month, day) from days since the unix epoch.
// see http://howardhinnant.github.io/date_algorithms.html#civil_from_days
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (year + i64::from(month <= 2), month, day)
}
