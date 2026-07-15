//! Driver for the on-board PCF8563 real-time clock.
//!
//! The chip is battery backed, so unlike the ESP32's internal RTC it keeps
//! time across cold boots, reflashes, and full power-offs — only a battery
//! pull (or first boot) loses it, which the chip reports via its
//! voltage-low flag. Runs on the shared I2C bus.
//!
//! note: this and the PCF85063 both answer at 0x51 with an identically laid
//! out seconds..years block, but the block starts at a different register
//! (0x02 here vs 0x04) — reading it at the wrong base leaves the clock stuck
//! on what is really the hours register, ticking once an hour.

use esp_hal::{i2c::master::I2c, Blocking};

pub(crate) const ADDR: u8 = 0x51;
const REG_CTRL1: u8 = 0x00;
// seconds..years block: seconds, minutes, hours, days, weekdays, months,
// years, all BCD. bit 7 of the seconds register is the voltage-low flag,
// set on power loss and cleared by writing the register.
const REG_SECONDS: u8 = 0x02;
const VL_FLAG: u8 = 1 << 7;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Request {
    Read,
    Write(u64),
}

pub(crate) enum Response {
    OptU64(crate::Result<Option<u64>>),
    Unit(crate::Result<()>),
}

/// Dispatch a queued request against the owned i2c bus. Called from
/// `crate::i2c::Worker::dispatch`, which runs on the core that owns `i2c`.
pub(crate) fn dispatch(i2c: &mut I2c<'_, Blocking>, request: Request) -> Response {
    match request {
        Request::Read => Response::OptU64(read_unix_raw(i2c)),
        Request::Write(unix) => Response::Unit(set_unix_raw(i2c, unix)),
    }
}

/// Read the current UTC unix time in seconds from the battery-backed PCF8563
/// real-time clock, over the i2c worker's channel.
///
/// Returns `None` when the chip reports its clock integrity was lost (time
/// was lost to a power interruption and never set since) or when the stored
/// calendar values are implausible — a different chip variant or an
/// unprogrammed part must fall back to a network sync rather than set a
/// garbage clock.
pub fn read_unix() -> crate::Result<Option<u64>> {
    match crate::i2c::submit(crate::i2c::Request::Rtc(Request::Read)) {
        crate::i2c::Response::Rtc(Response::OptU64(r)) => r,
        _ => unreachable!("pcf8563: Read always answers Response::OptU64"),
    }
}

/// Set the clock to a UTC unix time, starting the oscillator and clearing
/// the voltage-low flag so the time reads back as valid.
pub fn set_unix(unix: u64) -> crate::Result<()> {
    match crate::i2c::submit(crate::i2c::Request::Rtc(Request::Write(unix))) {
        crate::i2c::Response::Rtc(Response::Unit(r)) => r,
        _ => unreachable!("pcf8563: Write always answers Response::Unit"),
    }
}

fn read_unix_raw(i2c: &mut I2c<'_, Blocking>) -> crate::Result<Option<u64>> {
    let mut regs = [0u8; 7];
    i2c.write_read(ADDR, &[REG_SECONDS], &mut regs)
        .map_err(crate::Error::I2c)?;
    if regs[0] & VL_FLAG != 0 {
        return Ok(None);
    }

    let second = bcd_to_bin(regs[0] & 0x7F);
    let minute = bcd_to_bin(regs[1] & 0x7F);
    let hour = bcd_to_bin(regs[2] & 0x3F);
    let day = bcd_to_bin(regs[3] & 0x3F);
    let month = bcd_to_bin(regs[5] & 0x1F);
    let year = 2000 + u16::from(bcd_to_bin(regs[6]));

    if second > 59
        || minute > 59
        || hour > 23
        || !(1..=31).contains(&day)
        || !(1..=12).contains(&month)
        || year < 2025
    {
        return Ok(None);
    }

    let days = days_from_civil(i64::from(year), u32::from(month), u32::from(day));
    Ok(Some(
        days as u64 * 86_400 + u64::from(hour) * 3_600 + u64::from(minute) * 60 + u64::from(second),
    ))
}

fn set_unix_raw(i2c: &mut I2c<'_, Blocking>, unix: u64) -> crate::Result<()> {
    let (year, month, day) = civil_from_days((unix / 86_400) as i64);
    let rem = unix % 86_400;
    let (hour, minute, second) = (rem / 3_600, (rem / 60) % 60, rem % 60);

    // normal run mode, oscillator running (STOP cleared).
    i2c.write(ADDR, &[REG_CTRL1, 0x00])
        .map_err(crate::Error::I2c)?;
    i2c.write(
        ADDR,
        &[
            REG_SECONDS,
            bin_to_bcd(second as u8),
            bin_to_bcd(minute as u8),
            bin_to_bcd(hour as u8),
            bin_to_bcd(day as u8),
            // weekday, unused by this driver.
            0,
            bin_to_bcd(month as u8),
            bin_to_bcd((year - 2000).clamp(0, 99) as u8),
        ],
    )
    .map_err(crate::Error::I2c)
}

fn bcd_to_bin(value: u8) -> u8 {
    (value >> 4) * 10 + (value & 0x0F)
}

fn bin_to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

// gregorian calendar <-> days since the unix epoch (Howard Hinnant's civil
// calendar algorithms).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = i64::from(if month > 2 { month - 3 } else { month + 9 });
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

impl crate::i2c::Registered for crate::i2c::Addr<{ ADDR }> {}
