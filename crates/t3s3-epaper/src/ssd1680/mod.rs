//! Blocking driver for the SSD1680-based DEPG0213BN e-paper panel on the
//! LilyGO T3-S3 e-paper board (2.13", 122 x 250, black/white).
//!
//! The driver keeps a 1-bit framebuffer in RAM, implements `embedded-graphics`
//! [`DrawTarget`] so text and shapes can be drawn into it, and flushes it to
//! the panel with a full or partial (differential) refresh.
//!
//! It talks to the controller over a write-only `embedded-hal` SPI device
//! (which owns the CS line) plus three GPIOs: DC and RESET (outputs) and BUSY
//! (input).

mod command;
mod error;
mod lut;

use command as cmd;
use embedded_graphics_core::{
    Pixel,
    draw_target::DrawTarget,
    geometry::{OriginDimensions, Size},
    pixelcolor::BinaryColor,
};
use embedded_hal::{
    delay::DelayNs,
    digital::{InputPin, OutputPin},
    spi::SpiDevice,
};
pub use error::Error;

/// visible panel width in pixels (source axis).
const PANEL_WIDTH: i32 = 122;
/// panel height in pixels (gate axis).
const PANEL_HEIGHT: i32 = 250;
/// bytes per framebuffer row: 122 px padded up to 128 / 8 = 16.
const ROW_BYTES: usize = 16;
/// full framebuffer size, 16 bytes/row x 250 rows.
const FB_SIZE: usize = ROW_BYTES * PANEL_HEIGHT as usize;

/// a full refresh can take well over five seconds; poll up to ~40 s as a safety
/// net.
const BUSY_POLL_ITERS: u32 = 4_000;
const BUSY_POLL_MS: u32 = 10;

/// Display rotation, applied when mapping `embedded-graphics` coordinates into
/// the framebuffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Rotation {
    #[default]
    Rotate0,
    Rotate90,
    Rotate180,
    Rotate270,
}

/// Blocking SSD1680 e-paper driver with an in-RAM framebuffer.
pub struct Display<SPI, DC, RST, BUSY, DELAY> {
    spi: SPI,
    dc: DC,
    rst: RST,
    busy: BUSY,
    delay: DELAY,
    framebuffer: [u8; FB_SIZE],
    rotation: Rotation,
}

impl<SPI, DC, RST, BUSY, DELAY, PE> Display<SPI, DC, RST, BUSY, DELAY>
where
    SPI: SpiDevice<u8>,
    DC: OutputPin<Error = PE>,
    RST: OutputPin<Error = PE>,
    BUSY: InputPin<Error = PE>,
    DELAY: DelayNs,
{
    /// Create a driver from the SPI device, control pins and a delay source.
    ///
    /// The framebuffer starts blank (white). Call [`init`](Self::init) before
    /// drawing.
    pub fn new(spi: SPI, dc: DC, rst: RST, busy: BUSY, delay: DELAY) -> Self {
        Self {
            spi,
            dc,
            rst,
            busy,
            delay,
            framebuffer: [0xFF; FB_SIZE],
            rotation: Rotation::Rotate0,
        }
    }

    /// Set the rotation used when drawing into the framebuffer.
    pub fn set_rotation(&mut self, rotation: Rotation) {
        self.rotation = rotation;
    }

    /// Reset and configure the panel for full-screen operation.
    pub fn init(&mut self) -> Result<(), Error<SPI::Error, PE>> {
        self.reset()?;
        self.command(cmd::SW_RESET)?;
        self.wait_busy()?;

        // driver output control: 0x0127 = 295 mux lines, scan setting 0x00.
        self.cmd_data(cmd::DRIVER_OUTPUT_CONTROL, &[0x27, 0x01, 0x00])?;
        // data entry mode: x increment, y increment.
        self.cmd_data(cmd::DATA_ENTRY_MODE, &[0x03])?;
        self.cmd_data(cmd::BORDER_WAVEFORM, &[0x05])?;
        self.cmd_data(cmd::DISPLAY_UPDATE_CONTROL_1, &[0x00, 0x80])?;
        // use the built-in temperature sensor for waveform selection.
        self.cmd_data(cmd::TEMP_SENSOR_CONTROL, &[0x80])?;
        self.set_ram_window()?;
        self.wait_busy()
    }

    /// Set every pixel in the framebuffer to one colour.
    pub fn fill(&mut self, color: BinaryColor) {
        let byte = match color {
            BinaryColor::On => 0x00,  // black
            BinaryColor::Off => 0xFF, // white
        };
        self.framebuffer = [byte; FB_SIZE];
    }

    /// Flush the framebuffer with a full refresh (clean, but flickers and takes
    /// a few seconds).
    pub fn refresh(&mut self) -> Result<(), Error<SPI::Error, PE>> {
        // keep the "old" ram bank in sync so a later partial refresh can diff
        // against the currently displayed image.
        self.write_ram(cmd::WRITE_RAM_BW)?;
        self.write_ram(cmd::WRITE_RAM_RED)?;
        self.cmd_data(cmd::DISPLAY_UPDATE_CONTROL_2, &[cmd::UPDATE_SEQUENCE_FULL])?;
        self.command(cmd::MASTER_ACTIVATION)?;
        self.wait_busy()
    }

    /// Flush the framebuffer with a partial (differential) refresh: faster and
    /// flicker-free, at the cost of some ghosting over many updates.
    ///
    /// A full [`refresh`](Self::refresh) must have run at least once first so
    /// the panel has a reference image to diff against.
    pub fn refresh_partial(&mut self) -> Result<(), Error<SPI::Error, PE>> {
        self.cmd_data(cmd::WRITE_LUT, &lut::PARTIAL)?;
        self.write_ram(cmd::WRITE_RAM_BW)?;
        self.cmd_data(
            cmd::DISPLAY_UPDATE_CONTROL_2,
            &[cmd::UPDATE_SEQUENCE_PARTIAL],
        )?;
        self.command(cmd::MASTER_ACTIVATION)?;
        self.wait_busy()?;
        // resync the reference bank with what is now on screen.
        self.write_ram(cmd::WRITE_RAM_RED)
    }

    /// Put the panel into deep sleep (retaining the displayed image). Call
    /// [`init`](Self::init) again to wake it.
    pub fn sleep(&mut self) -> Result<(), Error<SPI::Error, PE>> {
        self.cmd_data(cmd::DEEP_SLEEP, &[0x01])
    }

    // -- framebuffer --------------------------------------------------------

    fn set_pixel(&mut self, x: i32, y: i32, color: BinaryColor) {
        let (nx, ny) = match self.rotation {
            Rotation::Rotate0 => (x, y),
            Rotation::Rotate90 => (PANEL_WIDTH - 1 - y, x),
            Rotation::Rotate180 => (PANEL_WIDTH - 1 - x, PANEL_HEIGHT - 1 - y),
            Rotation::Rotate270 => (y, PANEL_HEIGHT - 1 - x),
        };
        if !(0..PANEL_WIDTH).contains(&nx) || !(0..PANEL_HEIGHT).contains(&ny) {
            return;
        }
        let index = ny as usize * ROW_BYTES + nx as usize / 8;
        let bit = 7 - (nx as usize % 8);
        match color {
            BinaryColor::On => self.framebuffer[index] &= !(1 << bit), // black
            BinaryColor::Off => self.framebuffer[index] |= 1 << bit,   // white
        }
    }

    // -- controller ---------------------------------------------------------

    fn set_ram_window(&mut self) -> Result<(), Error<SPI::Error, PE>> {
        // x range is expressed in bytes (8 px each); y range in gate lines.
        self.cmd_data(cmd::SET_RAM_X_RANGE, &[0x00, (ROW_BYTES - 1) as u8])?;
        let y_end = PANEL_HEIGHT - 1;
        self.cmd_data(
            cmd::SET_RAM_Y_RANGE,
            &[0x00, 0x00, (y_end & 0xFF) as u8, (y_end >> 8) as u8],
        )?;
        self.cmd_data(cmd::SET_RAM_X_COUNTER, &[0x00])?;
        self.cmd_data(cmd::SET_RAM_Y_COUNTER, &[0x00, 0x00])
    }

    fn write_ram(&mut self, ram_command: u8) -> Result<(), Error<SPI::Error, PE>> {
        self.set_ram_window()?;
        self.command(ram_command)?;
        self.dc.set_high().map_err(Error::Pin)?;
        // disjoint borrows: spi (mut) and framebuffer (shared) are distinct fields.
        self.spi.write(&self.framebuffer).map_err(Error::Spi)
    }

    fn reset(&mut self) -> Result<(), Error<SPI::Error, PE>> {
        self.rst.set_low().map_err(Error::Pin)?;
        self.delay.delay_ms(10);
        self.rst.set_high().map_err(Error::Pin)?;
        self.delay.delay_ms(10);
        self.wait_busy()
    }

    // -- low-level spi ------------------------------------------------------

    fn command(&mut self, command: u8) -> Result<(), Error<SPI::Error, PE>> {
        self.dc.set_low().map_err(Error::Pin)?;
        self.spi.write(&[command]).map_err(Error::Spi)
    }

    fn cmd_data(&mut self, command: u8, data: &[u8]) -> Result<(), Error<SPI::Error, PE>> {
        self.command(command)?;
        self.dc.set_high().map_err(Error::Pin)?;
        self.spi.write(data).map_err(Error::Spi)
    }

    fn wait_busy(&mut self) -> Result<(), Error<SPI::Error, PE>> {
        // the ssd1680 holds busy high while working.
        for _ in 0..BUSY_POLL_ITERS {
            if self.busy.is_low().map_err(Error::Pin)? {
                return Ok(());
            }
            self.delay.delay_ms(BUSY_POLL_MS);
        }
        Err(Error::Timeout)
    }
}

impl<SPI, DC, RST, BUSY, DELAY, PE> DrawTarget for Display<SPI, DC, RST, BUSY, DELAY>
where
    SPI: SpiDevice<u8>,
    DC: OutputPin<Error = PE>,
    RST: OutputPin<Error = PE>,
    BUSY: InputPin<Error = PE>,
    DELAY: DelayNs,
{
    type Color = BinaryColor;
    type Error = Error<SPI::Error, PE>;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(coord, color) in pixels {
            self.set_pixel(coord.x, coord.y, color);
        }
        Ok(())
    }
}

impl<SPI, DC, RST, BUSY, DELAY, PE> OriginDimensions for Display<SPI, DC, RST, BUSY, DELAY>
where
    SPI: SpiDevice<u8>,
    DC: OutputPin<Error = PE>,
    RST: OutputPin<Error = PE>,
    BUSY: InputPin<Error = PE>,
    DELAY: DelayNs,
{
    fn size(&self) -> Size {
        match self.rotation {
            Rotation::Rotate0 | Rotation::Rotate180 => {
                Size::new(PANEL_WIDTH as u32, PANEL_HEIGHT as u32)
            }
            Rotation::Rotate90 | Rotation::Rotate270 => {
                Size::new(PANEL_HEIGHT as u32, PANEL_WIDTH as u32)
            }
        }
    }
}
