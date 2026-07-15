//! Driver for the on-board PCA9555 IO expander.
//!
//! Shared by the display's panel-power sequencing (raw byte read/write via
//! `crate::i2c::{read_byte, write_byte}`, driven from `crate::ed047tc1`) and
//! the aux button, polled autonomously here via [`AuxButton`].

use esp_hal::{i2c::master::I2c, Blocking};

pub(crate) const ADDR: u8 = 0x20;
pub(crate) const REG_INPUT_PORT1: u8 = 1;
const BIT_BUTTON: u8 = 1 << 2;

/// The aux button, polled autonomously by `crate::i2c::Worker::run`.
pub(crate) struct AuxButton;

impl crate::i2c::PolledDevice for AuxButton {
    // a physical switch, not a hot input path, so it's polled far less
    // often than touch.
    const POLL_INTERVAL_US: u64 = 20_000;

    fn poll(&mut self, i2c: &mut I2c<'_, Blocking>) -> crate::Result<()> {
        let mut value = [0u8; 1];
        i2c.write_read(ADDR, &[REG_INPUT_PORT1], &mut value)
            .map_err(crate::Error::I2c)?;
        crate::i2c::set_aux_button(value[0] & BIT_BUTTON == 0);
        Ok(())
    }
}

impl crate::i2c::Registered for crate::i2c::Addr<{ ADDR }> {}
