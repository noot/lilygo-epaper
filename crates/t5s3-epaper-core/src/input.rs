use core::cell::RefCell;

use esp_hal::{
    gpio::{AnyPin, Flex, Input, InputConfig, Level, Output, OutputConfig, Pin, Pull, RtcPin},
    i2c::master::I2c,
    peripherals,
    Blocking,
};
use log::debug;

use crate::{ed047tc1::busy_delay, touchscreen::TouchState};

const GT911_ADDR_LOW: u8 = 0x5D;
const GT911_ADDR_HIGH: u8 = 0x14;
const GT911_PRODUCT_ID: u16 = 0x8140;
const GT911_CONFIG_VERSION: u16 = 0x8047;
const GT911_MODULE_SWITCH_1: u16 = 0x804D;
const GT911_CONFIG_CHKSUM: u16 = 0x80FF;
const GT911_CONFIG_FRESH: u16 = 0x8100;
const GT911_CONFIG_LENGTH: usize = 186;
const GT911_COMMAND: u16 = 0x8040;
const GT911_CMD_SLEEP: u8 = 0x05;
const GT911_POINT_INFO: u16 = 0x814E;
const GT911_POINT_1: u16 = 0x814F;
const GT911_X_RESOLUTION: u16 = 0x8146;
const GT911_Y_RESOLUTION: u16 = 0x8148;
const GT911_DEV_ID: u32 = 911;
// the auxiliary button hangs off the PCA9555 IO expander; reading its input
// port is stateless, so the expander is safely shared with the display's
// panel-power writer (the pin also powers up as an input by default).
const PCA9555_ADDR: u8 = 0x20;
const PCA9555_REG_INPUT_PORT1: u8 = 1;
const PCA_BIT_BUTTON: u8 = 1 << 2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Buttons {
    pub home: bool,
    pub auxiliary: bool,
    pub boot: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputState {
    pub touch: Option<TouchState>,
    pub buttons: Buttons,
}

pub struct PinConfig<'d> {
    pub touch_int: peripherals::GPIO3<'d>,
    pub touch_rst: peripherals::GPIO9<'d>,
    pub boot_btn: peripherals::GPIO0<'d>,
}

/// The board's input devices: the GT911 touch controller (which also reports
/// the circular home "button"), the auxiliary button on the IO expander, and
/// the boot button.
///
/// Runs on the shared I2C bus, independent of the display driver. The GT911
/// is probed lazily on the first [`Controller::state`] read; a failed probe is
/// reported but the buttons are still read, so a broken touch panel degrades
/// to button-only operation instead of taking all input down.
pub struct Controller<'a, 'd> {
    i2c: &'a RefCell<I2c<'d, Blocking>>,
    touch_rst: Output<'d>,
    touch_int: Flex<'d>,
    boot_btn: AnyPin<'d>,
    touch_initialized: bool,
    // a failed probe is latched so a missing/broken panel doesn't re-run the
    // slow reset dance on every poll; a reboot retries.
    touch_failed: bool,
    touch_addr: u8,
    touch_resolution: (u16, u16),
}

impl<'a, 'd> Controller<'a, 'd> {
    /// Create the input controller. The GT911 itself is initialized lazily on
    /// the first [`Controller::state`] call.
    pub fn new(bus: &'a crate::i2c::Bus<'d>, pins: PinConfig<'d>) -> Self {
        // sleep latched these pads (see `sleep`); release them before driving.
        pins.touch_rst.rtcio_pad_hold(false);
        pins.touch_int.rtcio_pad_hold(false);

        Controller {
            i2c: &bus.i2c,
            touch_rst: Output::new(pins.touch_rst, Level::High, OutputConfig::default()),
            touch_int: {
                let mut pin = Flex::new(pins.touch_int);
                pin.set_output_enable(false);
                pin.set_input_enable(true);
                pin.apply_input_config(&InputConfig::default());
                pin
            },
            boot_btn: pins.boot_btn.degrade(),
            touch_initialized: false,
            touch_failed: false,
            touch_addr: GT911_ADDR_LOW,
            touch_resolution: (0, 0),
        }
    }

    /// Read the current input state: touch points plus all three buttons.
    ///
    /// A failed touch probe (or a transient touch read error) yields an empty
    /// touch state rather than an error, so a broken panel degrades to
    /// button-only operation instead of taking all input down.
    pub fn state(&mut self) -> crate::Result<InputState> {
        let mut input = self.touch_state().unwrap_or_default();
        input.buttons.auxiliary = self.auxiliary_button_pressed()?;
        let boot_btn = Input::new(
            self.boot_btn.reborrow(),
            InputConfig::default().with_pull(Pull::Up),
        );
        input.buttons.boot = boot_btn.is_low();
        Ok(input)
    }

    /// Return the touchscreen resolution reported by the GT911 controller.
    pub fn touch_resolution(&self) -> (u16, u16) {
        self.touch_resolution
    }

    // put the GT911 to sleep and latch its pads for deep sleep; it lives on
    // the always-on 3.3 V rail and keeps scanning (~3-4 mA) otherwise.
    pub(crate) fn sleep(&mut self) -> crate::Result<()> {
        if !self.touch_initialized {
            return Ok(());
        }
        // the GT911 only latches the 0x05 sleep command with INT held low, so
        // assert INT first.
        self.touch_int.set_low();
        self.touch_int.set_output_enable(true);
        self.touch_int.set_input_enable(false);
        busy_delay(30_000);
        let cmd = self.write_register16(self.touch_addr, GT911_COMMAND, &[GT911_CMD_SLEEP]);

        // The command alone is not enough: once the pin is released the INT
        // pull-up floats high, which is the GT911 wake trigger, so it resumes
        // scanning. Hold RST low instead, keeping the chip in reset for the
        // whole deep sleep. `touch_reset_for_address` resets it on the next boot.
        self.touch_rst.set_low();
        busy_delay(30_000);
        // SAFETY: GPIO9 is owned by `self.touch_rst`, which is driving it low
        // right now. `rtcio_pad_hold` only sets the LP_AON hold latch and does
        // not touch the output registers the live `Output` drives. The latch
        // freezes the pad low across deep sleep and is cleared by the matching
        // `rtcio_pad_hold(false)` in `new`.
        unsafe { peripherals::GPIO9::steal() }.rtcio_pad_hold(true);
        cmd
    }

    // hand the boot button back for use as the deep-sleep wake source.
    pub(crate) fn into_boot_button(self) -> AnyPin<'d> {
        self.boot_btn
    }

    fn auxiliary_button_pressed(&mut self) -> crate::Result<bool> {
        Ok(self.read_register(PCA9555_ADDR, PCA9555_REG_INPUT_PORT1)? & PCA_BIT_BUTTON == 0)
    }

    fn init_touch(&mut self) -> crate::Result<()> {
        debug!("touch init: probing GT911");
        self.touch_reset_for_address(GT911_ADDR_LOW)?;

        let mut product_id = [0u8; 4];
        if self
            .read_register16(GT911_ADDR_LOW, GT911_PRODUCT_ID, &mut product_id)
            .is_ok()
            && parse_gt911_chip_id(product_id) == GT911_DEV_ID
        {
            debug!(
                "touch init: addr 0x{:02X} product_id={:?}",
                GT911_ADDR_LOW, product_id
            );
            self.touch_addr = GT911_ADDR_LOW;
        } else {
            debug!(
                "touch init: addr 0x{:02X} probe failed product_id={:?}",
                GT911_ADDR_LOW, product_id
            );
            self.touch_reset_for_address(GT911_ADDR_HIGH)?;
            self.read_register16(GT911_ADDR_HIGH, GT911_PRODUCT_ID, &mut product_id)?;
            debug!(
                "touch init: addr 0x{:02X} product_id={:?}",
                GT911_ADDR_HIGH, product_id
            );
            self.touch_addr = GT911_ADDR_HIGH;
        }

        let chip_id = parse_gt911_chip_id(product_id);
        if chip_id != GT911_DEV_ID {
            debug!("touch init: unexpected chip id {}", chip_id);
            return Err(crate::Error::TouchInitFailed);
        }

        let x_res = self.touch_read_u16(GT911_X_RESOLUTION)?;
        let y_res = self.touch_read_u16(GT911_Y_RESOLUTION)?;
        // the GT911 reports an inconsistent resolution depending on whether it
        // has been configured before: zero on a cold boot, non-zero after a
        // deep-sleep wake (its config survives). that flips the coordinate
        // mapping below. the panel is a fixed 540x960, so pin it to keep touch
        // stable across boots.
        debug!(
            "touch init: reported resolution {}x{}, pinning to 540x960",
            x_res, y_res
        );
        self.touch_resolution = (
            crate::display::Display::HEIGHT,
            crate::display::Display::WIDTH,
        );
        self.touch_set_interrupt_mode_low_level_query()?;
        debug!(
            "touch init: resolution={}x{}",
            self.touch_resolution.0, self.touch_resolution.1
        );
        self.touch_initialized = true;

        Ok(())
    }

    fn ensure_touch(&mut self) -> crate::Result<()> {
        if self.touch_initialized {
            return Ok(());
        }
        if self.touch_failed {
            return Err(crate::Error::TouchInitFailed);
        }
        debug!("touch init: lazy init");
        if let Err(e) = self.init_touch() {
            self.touch_failed = true;
            return Err(e);
        }
        Ok(())
    }

    fn touch_reset_for_address(&mut self, address: u8) -> crate::Result<()> {
        self.touch_rst.set_low();
        busy_delay(30_000);

        match address {
            GT911_ADDR_HIGH => {
                self.touch_int.set_high();
            }
            _ => {
                self.touch_int.set_low();
            }
        }
        self.touch_int.set_output_enable(true);
        self.touch_int.set_input_enable(false);
        busy_delay(30_000);
        self.touch_rst.set_high();
        busy_delay(4_500_000);
        self.touch_int.set_output_enable(false);
        self.touch_int.set_input_enable(true);
        self.touch_int.apply_input_config(&InputConfig::default());
        busy_delay(5_000_000);
        Ok(())
    }

    fn touch_pressed(&self) -> bool {
        self.touch_int.is_low()
    }

    fn touch_read_u16(&mut self, reg: u16) -> crate::Result<u16> {
        let mut value = [0u8; 2];
        self.read_register16(self.touch_addr, reg, &mut value)?;
        Ok(u16::from_le_bytes(value))
    }

    fn touch_set_interrupt_mode_low_level_query(&mut self) -> crate::Result<()> {
        let mut value = [0u8; 1];
        self.read_register16(self.touch_addr, GT911_MODULE_SWITCH_1, &mut value)?;
        value[0] = (value[0] & 0xFC) | 0x02;
        self.write_register16(self.touch_addr, GT911_MODULE_SWITCH_1, &value)?;
        self.touch_reload_config()
    }

    fn touch_reload_config(&mut self) -> crate::Result<()> {
        let mut config = [0u8; GT911_CONFIG_LENGTH - 2];
        self.read_register16(self.touch_addr, GT911_CONFIG_VERSION, &mut config)?;
        let checksum = (!config
            .iter()
            .fold(0u8, |sum, value| sum.wrapping_add(*value)))
        .wrapping_add(1);
        self.write_register16(self.touch_addr, GT911_CONFIG_CHKSUM, &[checksum])?;
        self.write_register16(self.touch_addr, GT911_CONFIG_FRESH, &[0x01])?;
        Ok(())
    }

    fn touch_state(&mut self) -> crate::Result<InputState> {
        self.ensure_touch()?;

        if !self.touch_pressed() {
            return Ok(InputState::default());
        }

        let mut point_info = [0u8; 1];
        self.read_register16(self.touch_addr, GT911_POINT_INFO, &mut point_info)?;
        let status = point_info[0];
        let home = status & 0x10 != 0;
        let count = status & 0x0F;
        let buffer_ready = status & 0x80 != 0;
        if !buffer_ready && count == 0 {
            return Ok(InputState::default());
        }
        debug!("touch state: point_info=0x{:02X}", status);
        self.write_register16(self.touch_addr, GT911_POINT_INFO, &[0x00])?;

        let mut input = InputState {
            buttons: Buttons {
                home,
                ..Buttons::default()
            },
            ..InputState::default()
        };

        if count == 0 {
            return Ok(input);
        }

        let read_count = count.min(5) as usize;
        let mut buffer = [0u8; 39];
        self.read_register16(self.touch_addr, GT911_POINT_1, &mut buffer)?;

        let mut state = TouchState {
            count: read_count as u8,
            ..TouchState::default()
        };

        for i in 0..read_count {
            let offset = i * 8;
            state.points[i].id = buffer[offset];
            let raw_x = u16::from_le_bytes([buffer[offset + 1], buffer[offset + 2]]);
            let raw_y = u16::from_le_bytes([buffer[offset + 3], buffer[offset + 4]]);
            let (x, y) = if self.touch_resolution == (540, 960) {
                let x = (u32::from(raw_y) * u32::from(crate::display::Display::WIDTH - 1)
                    / u32::from(self.touch_resolution.1 - 1)) as u16;
                let y = (u32::from(
                    self.touch_resolution
                        .0
                        .saturating_sub(1)
                        .saturating_sub(raw_x),
                ) * u32::from(crate::display::Display::HEIGHT - 1)
                    / u32::from(self.touch_resolution.0 - 1)) as u16;
                (x, y)
            } else if self.touch_resolution.0 > 1 && self.touch_resolution.1 > 1 {
                let x = (u32::from(raw_x) * u32::from(crate::display::Display::WIDTH - 1)
                    / u32::from(self.touch_resolution.0 - 1)) as u16;
                let y = (u32::from(raw_y) * u32::from(crate::display::Display::HEIGHT - 1)
                    / u32::from(self.touch_resolution.1 - 1)) as u16;
                (x, y)
            } else {
                (raw_x, raw_y)
            };
            debug!(
                "touch point raw=({}, {}) mapped=({}, {})",
                raw_x, raw_y, x, y
            );
            state.points[i].x = x;
            state.points[i].y = y;
            state.points[i].size = u16::from_le_bytes([buffer[offset + 5], buffer[offset + 6]]);
        }

        input.touch = Some(state);

        Ok(input)
    }

    fn read_register(&mut self, device: u8, reg: u8) -> crate::Result<u8> {
        let mut value = [0u8; 1];
        self.i2c
            .borrow_mut()
            .write_read(device, &[reg], &mut value)
            .map_err(crate::Error::I2c)?;
        Ok(value[0])
    }

    fn write_register16(&mut self, device: u8, reg: u16, payload: &[u8]) -> crate::Result<()> {
        let mut buffer = [0u8; 41];
        let len = payload.len() + 2;
        buffer[0] = (reg >> 8) as u8;
        buffer[1] = reg as u8;
        buffer[2..len].copy_from_slice(payload);
        self.i2c
            .borrow_mut()
            .write(device, &buffer[..len])
            .map_err(crate::Error::I2c)
    }

    fn read_register16(&mut self, device: u8, reg: u16, payload: &mut [u8]) -> crate::Result<()> {
        let reg = [(reg >> 8) as u8, reg as u8];
        self.i2c
            .borrow_mut()
            .write_read(device, &reg, payload)
            .map_err(crate::Error::I2c)
    }
}

fn parse_gt911_chip_id(product_id: [u8; 4]) -> u32 {
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
