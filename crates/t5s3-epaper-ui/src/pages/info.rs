use alloc::{format, string::String};
use core::{fmt::Write as _, time::Duration};

use embedded_graphics::{
    mono_font::{
        ascii::{FONT_9X15, FONT_9X18_BOLD},
        MonoTextStyle,
    },
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use esp_hal::time::Instant;
use t5s3_epaper_core::{bq25896, bq27220, Clock, Display};

use crate::{
    fmt::FmtBuf,
    layout::{screen_to_native_rect, SCREEN_W},
    widgets::draw_back_button,
};

const INFO_TOP: i32 = 210;
const INFO_H: u32 = 480;

// shown on the info page.
const MODEL_NAME: &str = "LilyGo T5 S3 Paper Pro";
// loop ticks between info-page refreshes (~50ms per tick), so uptime/temp tick.
pub(crate) const INFO_REFRESH_TICKS: u16 = 40;

// format a duration in seconds as a compact "1h 23m" / "5m 12s" / "8s".
fn format_duration(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

// the live system stats shown on the info page.
pub(crate) struct Info {
    voltage: f32,
    temp: i8,
    uptime: u64,
    since_sync: Option<u64>,
    // None when the charger read failed; its rows then show "-".
    charger: Option<bq25896::Status>,
    // None when the battery is not charging (or the read failed).
    full_in: Option<Duration>,
    // None when the fuel gauge read failed; its row then shows "-".
    gauge: Option<bq27220::Diagnostics>,
}

// read the live system stats for the info page: battery voltage, panel
// temperature, uptime since boot, time since the last clock sync (None if
// it has not synced this power cycle), and the BQ25896 charger status.
pub(crate) fn read_info(display: &mut Display, clock: &mut Clock) -> Info {
    let voltage = display.battery_voltage().unwrap_or(0.0);
    let temp = display.panel_temperature().unwrap_or(0);
    let uptime = Instant::now().duration_since_epoch().as_micros() / 1_000_000;
    let now_secs = clock.now_us() / 1_000_000;
    let last_sync = unsafe { crate::LAST_SYNC_UNIX };
    let since_sync = if last_sync > 0 && now_secs >= last_sync {
        Some(now_secs - last_sync)
    } else {
        None
    };
    let charger = match display.charger_status() {
        Ok(status) => Some(status),
        Err(e) => {
            esp_println::println!("info: charger status read failed: {e}");
            None
        }
    };
    let full_in = match display.battery_time_to_full() {
        Ok(time) => time,
        Err(e) => {
            esp_println::println!("info: time-to-full read failed: {e}");
            None
        }
    };
    let gauge = match display.fuel_gauge_diagnostics() {
        Ok(diagnostics) => Some(diagnostics),
        Err(e) => {
            esp_println::println!("info: fuel gauge read failed: {e}");
            None
        }
    };
    Info {
        voltage,
        temp,
        uptime,
        since_sync,
        charger,
        full_in,
        gauge,
    }
}

// the label/value rows, drawn over a white fill so the periodic refresh cleanly
// replaces the previous values.
pub(crate) fn draw_info_values(display: &mut Display, info: &Info) {
    Rectangle::new(Point::new(40, INFO_TOP), Size::new(460, INFO_H))
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(display)
        .ok();

    // both black: this area is repainted with the 1-bit DU waveform on the
    // periodic refresh, which can't render a grey tone. the bold value font is
    // what sets values apart from labels.
    let label = MonoTextStyle::new(&FONT_9X15, Gray4::BLACK);
    let value = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    let v_int = info.voltage as u32;
    let v_frac = ((info.voltage - v_int as f32) * 100.0) as u32;

    let mut volt = FmtBuf::<16>::new();
    write!(volt, "{v_int}.{v_frac:02} V").ok();
    let mut tmp = FmtBuf::<16>::new();
    write!(tmp, "{} C", info.temp).ok();
    let uptime = format_duration(info.uptime);
    let synced = match info.since_sync {
        Some(s) => {
            let mut b = FmtBuf::<24>::new();
            write!(b, "{} ago", format_duration(s)).ok();
            b
        }
        None => {
            let mut b = FmtBuf::<24>::new();
            write!(b, "never").ok();
            b
        }
    };

    let mut input = FmtBuf::<24>::new();
    let mut current = FmtBuf::<16>::new();
    let charge = match &info.charger {
        Some(c) => {
            let source = match c.vbus {
                bq25896::VbusStatus::NoInput => "none",
                bq25896::VbusStatus::UsbHost => "USB",
                bq25896::VbusStatus::Adapter => "adapter",
                bq25896::VbusStatus::Otg => "OTG",
                bq25896::VbusStatus::Unknown => "unknown",
            };
            if c.vbus_mv > 0 {
                let mv = c.vbus_mv as u32;
                write!(input, "{source} {}.{:02} V", mv / 1000, (mv % 1000) / 10).ok();
            } else {
                write!(input, "{source}").ok();
            }
            write!(current, "{} mA", c.charge_ma).ok();
            match c.charge {
                bq25896::ChargeStatus::NotCharging => "not charging",
                bq25896::ChargeStatus::PreCharge => "pre-charge",
                bq25896::ChargeStatus::FastCharge => "fast charge",
                bq25896::ChargeStatus::Done => "done",
            }
        }
        None => {
            write!(input, "-").ok();
            write!(current, "-").ok();
            "-"
        }
    };

    let full_in = match info.full_in {
        Some(d) => format_duration(d.as_secs()),
        None => String::from("-"),
    };
    let mut capacity = FmtBuf::<24>::new();
    match &info.gauge {
        Some(g) => write!(capacity, "{}/{} mAh", g.remaining_mah, g.full_charge_mah).ok(),
        None => write!(capacity, "-").ok(),
    };

    let rows = [
        ("Model", MODEL_NAME),
        ("Battery", volt.as_str()),
        ("Temp", tmp.as_str()),
        ("Uptime", uptime.as_str()),
        ("Synced", synced.as_str()),
        ("Charge", charge),
        ("Input", input.as_str()),
        ("Current", current.as_str()),
        ("Full in", full_in.as_str()),
        ("Capacity", capacity.as_str()),
    ];
    let mut y = INFO_TOP + 36;
    for (name, val) in rows {
        Text::new(name, Point::new(70, y), label).draw(display).ok();
        Text::new(val, Point::new(250, y), value).draw(display).ok();
        y += 48;
    }
}

pub(crate) fn draw_info_screen(display: &mut Display, info: &Info) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    draw_back_button(display);
    Text::with_alignment(
        "Info",
        Point::new(SCREEN_W / 2, 120),
        bold,
        Alignment::Center,
    )
    .draw(display)
    .ok();
    draw_info_values(display, info);
}

pub(crate) fn info_values_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(40, INFO_TOP, 460, INFO_H as i32)
}
