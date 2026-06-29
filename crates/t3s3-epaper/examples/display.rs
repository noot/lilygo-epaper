//! Display example: draw text on the e-paper, then demonstrate a partial
//! update.
//!
//! Flash with `cargo run --example display` (requires the `esp` toolchain +
//! espflash).

#![no_std]
#![no_main]

use embedded_graphics::{
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
use t3s3_epaper::ssd1680::{Display, Rotation};

esp_bootloader_esp_idf::esp_app_desc!();

#[main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());

    // e-paper spi bus: sclk=14, mosi=11, cs=15 (see board module). no miso.
    let spi = Spi::new(
        peripherals.SPI2,
        SpiConfig::default()
            .with_frequency(Rate::from_mhz(4))
            .with_mode(Mode::_0),
    )
    .unwrap()
    .with_sck(peripherals.GPIO14)
    .with_mosi(peripherals.GPIO11);

    let cs = Output::new(peripherals.GPIO15, Level::High, OutputConfig::default());
    let spi_dev = ExclusiveDevice::new(spi, cs, Delay::new()).unwrap();

    let dc = Output::new(peripherals.GPIO16, Level::Low, OutputConfig::default());
    let rst = Output::new(peripherals.GPIO47, Level::High, OutputConfig::default());
    let busy = Input::new(
        peripherals.GPIO48,
        InputConfig::default().with_pull(Pull::None),
    );

    let mut display = Display::new(spi_dev, dc, rst, busy, Delay::new());
    display.set_rotation(Rotation::Rotate270); // landscape, 250 x 122
    display.init().unwrap();
    println!("ssd1680 ready");

    let title = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
    let body = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
    let rule = PrimitiveStyle::with_stroke(BinaryColor::On, 1);

    // initial full refresh with the static layout.
    draw_frame(&mut display, title, body, rule, 0);
    display.refresh().unwrap();
    println!("full refresh done");

    // then update the counter line with fast partial refreshes.
    let mut delay = Delay::new();
    let mut counter: u32 = 1;
    loop {
        delay.delay_ms(5_000);
        draw_frame(&mut display, title, body, rule, counter);
        display.refresh_partial().unwrap();
        println!("partial refresh #{counter}");
        counter = counter.wrapping_add(1);
    }
}

/// redraw the whole frame into the framebuffer with the given counter value.
fn draw_frame<D>(
    display: &mut D,
    title: MonoTextStyle<'_, BinaryColor>,
    body: MonoTextStyle<'_, BinaryColor>,
    rule: PrimitiveStyle<BinaryColor>,
    counter: u32,
) where
    D: DrawTarget<Color = BinaryColor>,
{
    let _ = display.clear(BinaryColor::Off);
    let _ = Text::new("LilyGO T3-S3", Point::new(8, 24), title).draw(display);
    let _ = Line::new(Point::new(8, 32), Point::new(242, 32))
        .into_styled(rule)
        .draw(display);
    let _ = Text::new("SX1262 LoRa + e-paper", Point::new(8, 50), body).draw(display);

    let mut text = *b"updates: 0000";
    write_dec(&mut text[9..], counter);
    if let Ok(s) = core::str::from_utf8(&text) {
        let _ = Text::new(s, Point::new(8, 80), body).draw(display);
    }
}

/// write `value` as 4 decimal digits into `out`.
fn write_dec(out: &mut [u8], value: u32) {
    let mut v = value % 10_000;
    for byte in out.iter_mut().take(4).rev() {
        *byte = b'0' + (v % 10) as u8;
        v /= 10;
    }
}
