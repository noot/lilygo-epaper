#![no_std]
#![no_main]

extern crate alloc;
extern crate t5s3_epaper_core;

use core::format_args;

use embedded_graphics::prelude::*;
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use esp_backtrace as _;
use esp_hal::{delay::Delay, main};
use t5s3_epaper_core::{
    display::Rectangle,
    lora::{Config, Lora},
    lora_pin_config,
    pin_config,
    Display,
};
use u8g2_fonts::FontRenderer;

static FONT: FontRenderer = FontRenderer::new::<u8g2_fonts::fonts::u8g2_font_spleen12x24_mr>();

esp_bootloader_esp_idf::esp_app_desc!();

#[main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default();
    let config = config.with_cpu_clock(esp_hal::clock::CpuClock::_240MHz);
    let peripherals = esp_hal::init(config);

    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);

    let i2c_worker = t5s3_epaper_core::i2c::Worker::new(
        peripherals.I2C0,
        peripherals.GPIO39,
        peripherals.GPIO40,
        t5s3_epaper_core::touch_pin_config!(peripherals),
    )
    .expect("to build i2c worker");
    static mut I2C_CORE_STACK: esp_hal::system::Stack<16384> = esp_hal::system::Stack::new();
    let mut cpu_control = esp_hal::system::CpuControl::new(peripherals.CPU_CTRL);
    let i2c_core_guard = cpu_control
        .start_app_core(
            unsafe { &mut *core::ptr::addr_of_mut!(I2C_CORE_STACK) },
            move || i2c_worker.run(),
        )
        .expect("to start the i2c worker on the second core");
    core::mem::forget(i2c_core_guard);

    let mut display = Display::new(
        pin_config!(peripherals),
        peripherals.DMA_CH0,
        peripherals.LCD_CAM,
        peripherals.RMT,
    )
    .expect("to initialize display");

    let delay = Delay::new();

    display.power_on().expect("to power on display");
    // LoRa and GPS share the VCC3V3 rail enabled by Display::new(); the
    // official firmware waits 1.5 s after power-on before talking to the radio.
    delay.delay_millis(1500);
    display.clear().expect("to clear screen");

    let bus = t5s3_epaper_core::spi::Bus::new(
        peripherals.SPI2,
        peripherals.GPIO14,
        peripherals.GPIO13,
        peripherals.GPIO21,
        peripherals.GPIO12,
        peripherals.GPIO46,
    )
    .expect("to build spi bus");
    let mut radio = Lora::new(&bus, lora_pin_config!(peripherals), &Config::default())
        .expect("to initialize LoRa radio");

    let text_area = Rectangle {
        x: 40,
        y: 60,
        width: 880,
        height: 200,
    };

    let mut buf = [0u8; 64];
    let mut count: u32 = 0;
    loop {
        match radio.receive(&mut buf, 5000) {
            Ok(Some(len)) => {
                count = count.wrapping_add(1);
                let text = core::str::from_utf8(&buf[..len]).unwrap_or("<binary>");
                let rssi = radio.rssi();
                let snr = radio.snr();
                esp_println::println!("lora rx: {} (rssi {} dBm, snr {} dB)", text, rssi, snr);

                FONT.render_aligned(
                    format_args!(
                        "LoRa RX @ 915 MHz\n\nrecv:  {}\nrssi:  {} dBm\nsnr:   {} dB\ncount: {}",
                        text, rssi, snr, count
                    ),
                    Point::new(60, 100),
                    u8g2_fonts::types::VerticalPosition::Baseline,
                    u8g2_fonts::types::HorizontalAlignment::Left,
                    u8g2_fonts::types::FontColor::WithBackground {
                        fg: Gray4::BLACK,
                        bg: Gray4::WHITE,
                    },
                    &mut display,
                )
                .expect("to render rx data");
                display
                    .flush_partial_fast(text_area)
                    .expect("to flush rx data");
            }
            Ok(None) => esp_println::println!("lora rx: listening..."),
            Err(e) => esp_println::println!("lora rx error: {}", e),
        }
    }
}
