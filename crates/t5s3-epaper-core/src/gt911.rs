//! Driver for the on-board GT911 capacitive touch controller.
//!
//! Owns its dedicated reset/interrupt pins in addition to the shared i2c
//! bus, plus its own debounced press/release state. Implements
//! [`crate::i2c::PolledDevice`], so [`crate::i2c::Worker::run`] samples it
//! on its own schedule without needing to know anything GT911-specific.
//!
//! The datasheet claims a 5-20ms configurable coordinate refresh cycle
//! (register 0x8055), but in practice (confirmed via the Goodix developer
//! forum: "GT911 - Touch Release Delay") the chip needs a real ~13ms after
//! the host clears the buffer-status register (0x814E) before a fresh
//! sample is ready. Polling faster than that floor re-reads a register the
//! chip may still be mid-update on, which reads as a one-sample "released"
//! blip in the middle of a real hold — exactly what produced duplicated
//! keystrokes before this poll interval and the debounce below were added.

use esp_hal::{
    gpio::{Flex, InputConfig, Level, Output, OutputConfig, RtcPin},
    i2c::master::I2c,
    peripherals,
    Blocking,
};

use crate::ed047tc1::busy_delay;

pub(crate) const ADDR_LOW: u8 = 0x5D;
pub(crate) const ADDR_HIGH: u8 = 0x14;
const PRODUCT_ID: u16 = 0x8140;
const CONFIG_VERSION: u16 = 0x8047;
const MODULE_SWITCH_1: u16 = 0x804D;
const CONFIG_CHKSUM: u16 = 0x80FF;
const CONFIG_FRESH: u16 = 0x8100;
const CONFIG_LENGTH: usize = 186;
const COMMAND: u16 = 0x8040;
const CMD_SLEEP: u8 = 0x05;
const POINT_INFO: u16 = 0x814E;
const POINT_1: u16 = 0x814F;
const X_RESOLUTION: u16 = 0x8146;
const Y_RESOLUTION: u16 = 0x8148;
const DEV_ID: u32 = 911;

// require this many consecutive same-state polls before flipping
// press/release state, so a single stale/torn read can't split one physical
// tap into two, or end one early. a press fires immediately on the first
// read that reports it; only release is debounced, since debouncing press
// instead would just delay the duplicate rather than prevent it. at the
// 15ms poll interval this adds at most ~30ms to release detection, well
// under a perceptible typing delay.
const DEBOUNCE_SAMPLES: u8 = 2;

/// Pins the GT911 needs beyond the shared i2c bus.
pub struct TouchPinConfig<'d> {
    pub touch_int: peripherals::GPIO3<'d>,
    pub touch_rst: peripherals::GPIO9<'d>,
}

#[derive(Default)]
struct Reading {
    home: bool,
    touch: Option<crate::touchscreen::TouchState>,
}

/// Owns the GT911's reset/interrupt pins and its debounced press/release
/// state. Built on core 0 during single-threaded boot, then moved wholesale
/// into [`crate::i2c::Worker::run`] on the second core.
pub(crate) struct Gt911<'d> {
    rst: Output<'d>,
    int: Flex<'d>,
    // a failed probe is latched so a missing/broken panel doesn't re-run the
    // slow reset dance on every poll; a reboot retries.
    initialized: bool,
    failed: bool,
    addr: u8,
    resolution: (u16, u16),
    touch_active: bool,
    touch_streak: u8,
    home_active: bool,
    home_streak: u8,
}

impl<'d> Gt911<'d> {
    /// The controller itself is initialized lazily on the first poll.
    pub(crate) fn new(pins: TouchPinConfig<'d>) -> Self {
        // sleep latched these pads (see `sleep`); release them before driving.
        pins.touch_rst.rtcio_pad_hold(false);
        pins.touch_int.rtcio_pad_hold(false);

        Gt911 {
            rst: Output::new(pins.touch_rst, Level::High, OutputConfig::default()),
            int: {
                let mut pin = Flex::new(pins.touch_int);
                pin.set_output_enable(false);
                pin.set_input_enable(true);
                pin.apply_input_config(&InputConfig::default());
                pin
            },
            initialized: false,
            failed: false,
            addr: ADDR_LOW,
            resolution: (0, 0),
            touch_active: false,
            touch_streak: 0,
            home_active: false,
            home_streak: 0,
        }
    }

    /// Put the GT911 to sleep and latch its pads for deep sleep; it lives on
    /// the always-on 3.3V rail and keeps scanning (~3-4mA) otherwise.
    ///
    /// Called only from [`crate::i2c::Worker::run`]'s deep-sleep handshake,
    /// the last i2c operation core 1 ever services.
    pub(crate) fn sleep(&mut self, i2c: &mut I2c<'_, Blocking>) -> crate::Result<()> {
        if !self.initialized {
            return Ok(());
        }
        // the GT911 only latches the 0x05 sleep command with INT held low, so
        // assert INT first.
        self.int.set_low();
        self.int.set_output_enable(true);
        self.int.set_input_enable(false);
        busy_delay(30_000);
        let cmd = self.write_register16(i2c, self.addr, COMMAND, &[CMD_SLEEP]);

        // The command alone is not enough: once the pin is released the INT
        // pull-up floats high, which is the GT911 wake trigger, so it resumes
        // scanning. Hold RST low instead, keeping the chip in reset for the
        // whole deep sleep. `reset_for_address` resets it on the next boot.
        self.rst.set_low();
        busy_delay(30_000);
        // SAFETY: GPIO9 is owned by `self.rst`, which is driving it low right
        // now. `rtcio_pad_hold` only sets the LP_AON hold latch and does not
        // touch the output registers the live `Output` drives. The latch
        // freezes the pad low across deep sleep and is cleared by the
        // matching `rtcio_pad_hold(false)` in `new`.
        unsafe { peripherals::GPIO9::steal() }.rtcio_pad_hold(true);
        cmd
    }

    fn ensure_initialized(&mut self, i2c: &mut I2c<'_, Blocking>) -> crate::Result<()> {
        if self.initialized {
            return Ok(());
        }
        if self.failed {
            return Err(crate::Error::TouchInitFailed);
        }
        log::debug!("touch init: lazy init");
        if let Err(e) = self.init(i2c) {
            self.failed = true;
            return Err(e);
        }
        Ok(())
    }

    fn init(&mut self, i2c: &mut I2c<'_, Blocking>) -> crate::Result<()> {
        log::debug!("touch init: probing GT911");
        self.reset_for_address(ADDR_LOW);

        let mut product_id = [0u8; 4];
        if self
            .read_register16(i2c, ADDR_LOW, PRODUCT_ID, &mut product_id)
            .is_ok()
            && parse_chip_id(product_id) == DEV_ID
        {
            log::debug!(
                "touch init: addr 0x{:02X} product_id={:?}",
                ADDR_LOW,
                product_id
            );
            self.addr = ADDR_LOW;
        } else {
            log::debug!(
                "touch init: addr 0x{:02X} probe failed product_id={:?}",
                ADDR_LOW,
                product_id
            );
            self.reset_for_address(ADDR_HIGH);
            self.read_register16(i2c, ADDR_HIGH, PRODUCT_ID, &mut product_id)?;
            log::debug!(
                "touch init: addr 0x{:02X} product_id={:?}",
                ADDR_HIGH,
                product_id
            );
            self.addr = ADDR_HIGH;
        }

        let chip_id = parse_chip_id(product_id);
        if chip_id != DEV_ID {
            log::debug!("touch init: unexpected chip id {}", chip_id);
            return Err(crate::Error::TouchInitFailed);
        }

        let x_res = self.read_u16(i2c, X_RESOLUTION)?;
        let y_res = self.read_u16(i2c, Y_RESOLUTION)?;
        // the GT911 reports an inconsistent resolution depending on whether it
        // has been configured before: zero on a cold boot, non-zero after a
        // deep-sleep wake (its config survives). that flips the coordinate
        // mapping below. the panel is a fixed 540x960, so pin it to keep touch
        // stable across boots.
        log::debug!(
            "touch init: reported resolution {}x{}, pinning to 540x960",
            x_res,
            y_res
        );
        self.resolution = (
            crate::display::Display::HEIGHT,
            crate::display::Display::WIDTH,
        );
        self.set_interrupt_mode_low_level_query(i2c)?;
        log::debug!(
            "touch init: resolution={}x{}",
            self.resolution.0,
            self.resolution.1
        );
        self.initialized = true;

        Ok(())
    }

    fn reset_for_address(&mut self, address: u8) {
        self.rst.set_low();
        busy_delay(30_000);

        match address {
            ADDR_HIGH => {
                self.int.set_high();
            }
            _ => {
                self.int.set_low();
            }
        }
        self.int.set_output_enable(true);
        self.int.set_input_enable(false);
        busy_delay(30_000);
        self.rst.set_high();
        busy_delay(4_500_000);
        self.int.set_output_enable(false);
        self.int.set_input_enable(true);
        self.int.apply_input_config(&InputConfig::default());
        busy_delay(5_000_000);
    }

    fn pressed(&self) -> bool {
        self.int.is_low()
    }

    fn read_u16(&mut self, i2c: &mut I2c<'_, Blocking>, reg: u16) -> crate::Result<u16> {
        let mut value = [0u8; 2];
        self.read_register16(i2c, self.addr, reg, &mut value)?;
        Ok(u16::from_le_bytes(value))
    }

    fn set_interrupt_mode_low_level_query(
        &mut self,
        i2c: &mut I2c<'_, Blocking>,
    ) -> crate::Result<()> {
        let mut value = [0u8; 1];
        self.read_register16(i2c, self.addr, MODULE_SWITCH_1, &mut value)?;
        value[0] = (value[0] & 0xFC) | 0x02;
        self.write_register16(i2c, self.addr, MODULE_SWITCH_1, &value)?;
        self.reload_config(i2c)
    }

    fn reload_config(&mut self, i2c: &mut I2c<'_, Blocking>) -> crate::Result<()> {
        let mut config = [0u8; CONFIG_LENGTH - 2];
        self.read_register16(i2c, self.addr, CONFIG_VERSION, &mut config)?;
        let checksum = (!config
            .iter()
            .fold(0u8, |sum, value| sum.wrapping_add(*value)))
        .wrapping_add(1);
        self.write_register16(i2c, self.addr, CONFIG_CHKSUM, &[checksum])?;
        self.write_register16(i2c, self.addr, CONFIG_FRESH, &[0x01])?;
        Ok(())
    }

    fn read(&mut self, i2c: &mut I2c<'_, Blocking>) -> crate::Result<Reading> {
        self.ensure_initialized(i2c)?;

        if !self.pressed() {
            return Ok(Reading::default());
        }

        let mut point_info = [0u8; 1];
        self.read_register16(i2c, self.addr, POINT_INFO, &mut point_info)?;
        let status = point_info[0];
        let home = status & 0x10 != 0;
        let count = status & 0x0F;
        let buffer_ready = status & 0x80 != 0;
        if !buffer_ready && count == 0 {
            return Ok(Reading::default());
        }
        log::debug!("touch state: point_info=0x{:02X}", status);
        self.write_register16(i2c, self.addr, POINT_INFO, &[0x00])?;

        let mut reading = Reading { home, touch: None };

        if count == 0 {
            return Ok(reading);
        }

        let read_count = count.min(5) as usize;
        let mut buffer = [0u8; 39];
        self.read_register16(i2c, self.addr, POINT_1, &mut buffer)?;

        let mut state = crate::touchscreen::TouchState {
            count: read_count as u8,
            ..Default::default()
        };

        for i in 0..read_count {
            let offset = i * 8;
            state.points[i].id = buffer[offset];
            let raw_x = u16::from_le_bytes([buffer[offset + 1], buffer[offset + 2]]);
            let raw_y = u16::from_le_bytes([buffer[offset + 3], buffer[offset + 4]]);
            let (x, y) = if self.resolution == (540, 960) {
                let x = (u32::from(raw_y) * u32::from(crate::display::Display::WIDTH - 1)
                    / u32::from(self.resolution.1 - 1)) as u16;
                let y = (u32::from(self.resolution.0.saturating_sub(1).saturating_sub(raw_x))
                    * u32::from(crate::display::Display::HEIGHT - 1)
                    / u32::from(self.resolution.0 - 1)) as u16;
                (x, y)
            } else if self.resolution.0 > 1 && self.resolution.1 > 1 {
                let x = (u32::from(raw_x) * u32::from(crate::display::Display::WIDTH - 1)
                    / u32::from(self.resolution.0 - 1)) as u16;
                let y = (u32::from(raw_y) * u32::from(crate::display::Display::HEIGHT - 1)
                    / u32::from(self.resolution.1 - 1)) as u16;
                (x, y)
            } else {
                (raw_x, raw_y)
            };
            log::debug!(
                "touch point raw=({}, {}) mapped=({}, {})",
                raw_x,
                raw_y,
                x,
                y
            );
            state.points[i].x = x;
            state.points[i].y = y;
            state.points[i].size = u16::from_le_bytes([buffer[offset + 5], buffer[offset + 6]]);
        }

        reading.touch = Some(state);

        Ok(reading)
    }

    fn read_register16(
        &mut self,
        i2c: &mut I2c<'_, Blocking>,
        device: u8,
        reg: u16,
        payload: &mut [u8],
    ) -> crate::Result<()> {
        let reg = [(reg >> 8) as u8, reg as u8];
        i2c.write_read(device, &reg, payload)
            .map_err(crate::Error::I2c)
    }

    fn write_register16(
        &mut self,
        i2c: &mut I2c<'_, Blocking>,
        device: u8,
        reg: u16,
        payload: &[u8],
    ) -> crate::Result<()> {
        let mut buffer = [0u8; 41];
        let len = payload.len() + 2;
        buffer[0] = (reg >> 8) as u8;
        buffer[1] = reg as u8;
        buffer[2..len].copy_from_slice(payload);
        i2c.write(device, &buffer[..len]).map_err(crate::Error::I2c)
    }
}

impl<'d> crate::i2c::PolledDevice for Gt911<'d> {
    // 15ms keeps a safety margin over the chip's real ~13ms refresh floor
    // (see module doc) without adding noticeable input latency.
    const POLL_INTERVAL_US: u64 = 15_000;

    /// Poll the controller and publish any debounced tap/home edge to
    /// [`crate::i2c`]'s shared event queue.
    fn poll(&mut self, i2c: &mut I2c<'_, Blocking>) -> crate::Result<()> {
        let reading = self.read(i2c)?;

        if reading.home {
            self.home_streak = 0;
            if !self.home_active {
                self.home_active = true;
                crate::i2c::push_event(crate::i2c::Event::Home);
            }
        } else {
            self.home_streak = self.home_streak.saturating_add(1);
            if self.home_active && self.home_streak >= DEBOUNCE_SAMPLES {
                self.home_active = false;
            }
        }

        match reading.touch.and_then(|s| s.first_point()) {
            Some(point) => {
                self.touch_streak = 0;
                if !self.touch_active {
                    self.touch_active = true;
                    crate::i2c::push_event(crate::i2c::Event::Tap {
                        x: point.x,
                        y: point.y,
                    });
                }
            }
            None => {
                self.touch_streak = self.touch_streak.saturating_add(1);
                if self.touch_active && self.touch_streak >= DEBOUNCE_SAMPLES {
                    self.touch_active = false;
                }
            }
        }

        Ok(())
    }
}

impl crate::i2c::Registered for crate::i2c::Addr<{ ADDR_LOW }> {}
impl crate::i2c::Registered for crate::i2c::Addr<{ ADDR_HIGH }> {}

fn parse_chip_id(product_id: [u8; 4]) -> u32 {
    let mut value = 0u32;
    for digit in product_id {
        if digit == 0 {
            break;
        }
        if !digit.is_ascii_digit() {
            return 0;
        }
        value = value * 10 + (digit - b'0') as u32;
    }
    value
}
