//! Receive example: print every LoRa packet that arrives and show it (with RSSI
//! and SNR) on the e-paper display.
//!
//! Flash with `cargo run --example rx` (requires the `esp` toolchain +
//! espflash).

#![no_std]
#![no_main]

use core::fmt::Write as _;

use embedded_graphics::{
    draw_target::DrawTarget,
    mono_font::{
        MonoTextStyle,
        ascii::{FONT_6X10, FONT_10X20},
    },
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{Line, PrimitiveStyle},
    text::Text,
};
use embedded_hal::delay::DelayNs as _;
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_backtrace as _;
use esp_hal::{
    delay::Delay,
    gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull},
    main,
    spi::{
        Mode,
        master::{Config as SpiConfig, Spi},
    },
    time::Rate,
};
use esp_println::println;
use t3s3_epaper::{
    ssd1680::{Display, Rotation},
    sx1262::{Config, Sx1262},
};

esp_bootloader_esp_idf::esp_app_desc!();

/// do a clean full refresh every this many packets to clear partial-refresh
/// ghosting.
const FULL_REFRESH_EVERY: u32 = 10;

#[main]
fn main() -> ! {
    // NOTE: do NOT force CpuClock::max() — at 240 MHz esp-hal's fixed SPI input
    // delay mis-samples MISO on these GPIO-matrix pins. The default clock works.
    let peripherals = esp_hal::init(esp_hal::Config::default());

    // lora radio on its own spi bus: sck=5, mosi=6, miso=3, nss=7.
    let radio_spi = Spi::new(
        peripherals.SPI2,
        SpiConfig::default()
            .with_frequency(Rate::from_mhz(1))
            .with_mode(Mode::_0),
    )
    .unwrap()
    .with_sck(peripherals.GPIO5)
    .with_mosi(peripherals.GPIO6)
    .with_miso(peripherals.GPIO3);
    let radio_cs = Output::new(peripherals.GPIO7, Level::High, OutputConfig::default());
    let radio_rst = Output::new(peripherals.GPIO8, Level::High, OutputConfig::default());
    let radio_busy = Input::new(
        peripherals.GPIO34,
        InputConfig::default().with_pull(Pull::None),
    );
    let radio_dio1 = Input::new(
        peripherals.GPIO33,
        InputConfig::default().with_pull(Pull::None),
    );
    // power the radio's oscillator rail (gpio35); the factory firmware drives
    // this high before using the radio, else the xosc never starts
    // (XOSC_START_ERR) and rx times out. hold the handle so it stays driven.
    let _radio_pow = Output::new(peripherals.GPIO35, Level::High, OutputConfig::default());
    Delay::new().delay_ms(10);
    let mut radio = Sx1262::new(
        radio_spi,
        radio_cs,
        radio_rst,
        radio_busy,
        radio_dio1,
        Delay::new(),
        Config::default(),
    );
    radio.init().unwrap();
    println!(
        "sx1262 ready, listening at 915 MHz (status={:#04x}, device_errors={:#06x})",
        radio.status().unwrap(),
        radio.device_errors().unwrap()
    );

    // e-paper display on a second spi bus: sclk=14, mosi=11, cs=15.
    let disp_spi = Spi::new(
        peripherals.SPI3,
        SpiConfig::default()
            .with_frequency(Rate::from_mhz(4))
            .with_mode(Mode::_0),
    )
    .unwrap()
    .with_sck(peripherals.GPIO14)
    .with_mosi(peripherals.GPIO11);
    let disp_cs = Output::new(peripherals.GPIO15, Level::High, OutputConfig::default());
    let disp_dev = ExclusiveDevice::new(disp_spi, disp_cs, Delay::new()).unwrap();
    let disp_dc = Output::new(peripherals.GPIO16, Level::Low, OutputConfig::default());
    let disp_rst = Output::new(peripherals.GPIO47, Level::High, OutputConfig::default());
    let disp_busy = Input::new(
        peripherals.GPIO48,
        InputConfig::default().with_pull(Pull::None),
    );
    let mut display = Display::new(disp_dev, disp_dc, disp_rst, disp_busy, Delay::new());
    display.set_rotation(Rotation::Rotate270); // landscape, 250 x 122

    display.init().unwrap();
    render(&mut display, "LoRa RX", "915 MHz", "listening...", "");
    display.refresh().unwrap();

    let mut buf = [0u8; 255];
    let mut counter: u32 = 0;
    loop {
        match radio.receive(&mut buf) {
            Ok(info) => {
                let payload = &buf[..info.len];
                println!(
                    "rx {} bytes  rssi={} dBm  snr={} dB  data={:?}",
                    info.len, info.rssi_dbm, info.snr_db, payload
                );

                let mut line1 = FmtBuf::new();
                let _ = write!(line1, "#{counter}  {} bytes", info.len);
                let mut line2 = FmtBuf::new();
                let _ = write!(line2, "rssi {} dBm  snr {} dB", info.rssi_dbm, info.snr_db);
                let mut line3 = FmtBuf::new();
                match core::str::from_utf8(payload) {
                    Ok(text) => {
                        let _ = write!(line3, "msg: {text}");
                    }
                    Err(_) => {
                        let _ = write!(line3, "msg: <binary>");
                    }
                }
                render(
                    &mut display,
                    "LoRa RX",
                    line1.as_str(),
                    line2.as_str(),
                    line3.as_str(),
                );
                if counter.is_multiple_of(FULL_REFRESH_EVERY) {
                    display.refresh().unwrap();
                } else {
                    display.refresh_partial().unwrap();
                }
                counter = counter.wrapping_add(1);
            }
            Err(e) => {
                println!("rx error: {e:?}");
                let mut line = FmtBuf::new();
                let _ = write!(line, "error: {e:?}");
                render(&mut display, "LoRa RX", "listening...", line.as_str(), "");
                display.refresh_partial().unwrap();
            }
        }
    }
}

/// draw a title, a rule and up to three body lines into the framebuffer.
fn render<D>(display: &mut D, title: &str, line1: &str, line2: &str, line3: &str)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let title_style = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
    let body = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
    let rule = PrimitiveStyle::with_stroke(BinaryColor::On, 1);

    let _ = display.clear(BinaryColor::Off);
    let _ = Text::new(title, Point::new(8, 24), title_style).draw(display);
    let _ = Line::new(Point::new(8, 32), Point::new(242, 32))
        .into_styled(rule)
        .draw(display);
    let _ = Text::new(line1, Point::new(8, 52), body).draw(display);
    let _ = Text::new(line2, Point::new(8, 68), body).draw(display);
    let _ = Text::new(line3, Point::new(8, 84), body).draw(display);
}

/// a tiny fixed-capacity buffer that implements `core::fmt::Write` so `write!`
/// can format strings without an allocator.
struct FmtBuf {
    buf: [u8; 48],
    len: usize,
}

impl FmtBuf {
    fn new() -> Self {
        Self {
            buf: [0; 48],
            len: 0,
        }
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl core::fmt::Write for FmtBuf {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let n = bytes.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&bytes[..n]);
        self.len += n;
        Ok(())
    }
}
