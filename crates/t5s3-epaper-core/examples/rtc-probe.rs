#![no_std]
#![no_main]

//! identify the on-board rtc chip. the pcf85063 and pcf8563 both answer at
//! i2c 0x51 but their register maps are offset by two: seconds live at 0x04 on
//! the pcf85063 and at 0x02 on the pcf8563. read the register block twice a few
//! seconds apart and report which register is ticking at 1 hz — that is the
//! real seconds register, and it names the chip. reads only, nothing written.

extern crate alloc;
extern crate t5s3_epaper_core;

use esp_backtrace as _;
use esp_hal::{
    delay::Delay,
    i2c::master::{Config as I2cConfig, I2c},
    main,
    time::Rate,
};

const ADDR: u8 = 0x51;

esp_bootloader_esp_idf::esp_app_desc!();

fn dump(i2c: &mut I2c<'_, esp_hal::Blocking>, buf: &mut [u8; 16]) {
    match i2c.write_read(ADDR, &[0x00], buf) {
        Ok(()) => {
            esp_println::println!(
                "regs 0x00..0x0f: {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x}",
                buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
                buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
            );
        }
        Err(e) => esp_println::println!("i2c read failed: {e:?}"),
    }
}

#[main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(esp_hal::clock::CpuClock::_240MHz);
    let peripherals = esp_hal::init(config);
    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);

    let mut i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default().with_frequency(Rate::from_khz(100)),
    )
    .expect("to build i2c")
    .with_sda(peripherals.GPIO39)
    .with_scl(peripherals.GPIO40);

    let delay = Delay::new();

    esp_println::println!("rtc probe: reading 0x51, first sample");
    let mut first = [0u8; 16];
    dump(&mut i2c, &mut first);

    delay.delay_millis(3000);

    esp_println::println!("rtc probe: second sample (after ~3s)");
    let mut second = [0u8; 16];
    dump(&mut i2c, &mut second);

    // whichever register advanced by roughly three counts over the three-second
    // gap is the real seconds register.
    let d02 = second[0x02].wrapping_sub(first[0x02]);
    let d04 = second[0x04].wrapping_sub(first[0x04]);
    esp_println::println!("delta reg[0x02]={:#04x} delta reg[0x04]={:#04x}", d02, d04);
    if first[0x02] != second[0x02] {
        esp_println::println!("=> reg 0x02 is ticking: this is a PCF8563 (seconds at 0x02)");
    } else if first[0x04] != second[0x04] {
        esp_println::println!("=> reg 0x04 is ticking: this is a PCF85063 (seconds at 0x04)");
    } else {
        esp_println::println!(
            "=> nothing ticked: oscillator stopped or never set (both samples identical)"
        );
    }

    loop {
        delay.delay_millis(1000);
    }
}
