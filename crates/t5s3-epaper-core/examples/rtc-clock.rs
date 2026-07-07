#![no_std]
#![no_main]

extern crate alloc;
extern crate t5s3_epaper_core;

use core::{format_args, time::Duration};

use embedded_graphics::prelude::*;
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use esp_backtrace as _;
use esp_hal::{delay::Delay, main};
use t5s3_epaper_core::{pin_config, power, rtc::Clock, Display, DrawMode};
use u8g2_fonts::FontRenderer;

static FONT: FontRenderer = FontRenderer::new::<u8g2_fonts::fonts::u8g2_font_spleen16x32_mr>();
const INITIAL_RTC_TIME_US: u64 = 1_700_000_000_000_000;

esp_bootloader_esp_idf::esp_app_desc!();

fn format_uk_datetime(unix_seconds: u64) -> (i32, u32, u32, u32, u32, u32) {
    let days = (unix_seconds / 86_400) as i64;
    let seconds_of_day = (unix_seconds % 86_400) as u32;

    // Howard Hinnant's civil-from-days algorithm, using the Unix epoch.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = (yoe as i32) + era as i32 * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (mp + if mp < 10 { 3 } else { -9 }) as u32;
    year += if month <= 2 { 1 } else { 0 };

    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;

    (year, month, day, hour, minute, second)
}

#[main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(esp_hal::clock::CpuClock::_240MHz);
    let peripherals = esp_hal::init(config);

    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);

    let mut clock = Clock::new(peripherals.LPWR);
    let wake = power::wake_status();

    if !wake.woke_from_deep_sleep() {
        clock.set_now_us(INITIAL_RTC_TIME_US);
    }

    let now_us = clock.now_us();
    let (year, month, day, hour, minute, second) = format_uk_datetime(now_us / 1_000_000);
    let uptime = clock.uptime();

    let i2c_bus =
        t5s3_epaper_core::i2c::Bus::new(peripherals.I2C0, peripherals.GPIO39, peripherals.GPIO40)
            .expect("to build i2c bus");
    let mut display = Display::new(
        pin_config!(peripherals),
        &i2c_bus,
        peripherals.DMA_CH0,
        peripherals.LCD_CAM,
        peripherals.RMT,
    )
    .expect("to initialize display");
    let input_ctl = t5s3_epaper_core::input::Controller::new(
        &i2c_bus,
        t5s3_epaper_core::input_pin_config!(peripherals),
    );

    let delay = Delay::new();

    display.power_on().expect("to power on display");
    delay.delay_millis(20);
    display.clear().expect("to clear screen");

    FONT.render_aligned(
        format_args!(
            "RTC: {:02}/{:02}/{:04} {:02}:{:02}:{:02}\nRaw: {} us\nUptime: {} ms\nReset: {:?}\nWake: {:?}\nSleeping for 30s",
            day,
            month,
            year,
            hour,
            minute,
            second,
            now_us,
            uptime.as_millis(),
            wake.reset_reason,
            wake.wakeup_cause,
        ),
        Point::new(
            display.bounding_box().center().x,
            display.bounding_box().center().y - 40,
        ),
        u8g2_fonts::types::VerticalPosition::Baseline,
        u8g2_fonts::types::HorizontalAlignment::Center,
        u8g2_fonts::types::FontColor::WithBackground {
            fg: Gray4::BLACK,
            bg: Gray4::WHITE,
        },
        &mut display,
    )
    .expect("to render text");

    display
        .flush(DrawMode::BlackOnWhite)
        .expect("to flush to display");
    display.power_off().expect("to power off display");

    delay.delay_millis(100);

    let lpwr = clock.into_inner();
    display.deep_sleep(lpwr, input_ctl, Some(Duration::from_secs(30)));
}
