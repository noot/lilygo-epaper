use alloc::{boxed::Box, vec, vec::Vec};
use core::time::Duration;

use esp_hal::{delay::Delay, peripherals};
use log::*;

use crate::{ed047tc1, input::InputState, touchscreen::TouchState, Error, Result};

const CONTRAST_CYCLES_4BPP: &[u16; 15] = &[
    30, 30, 20, 20, 30, 30, 30, 40, 40, 50, 50, 50, 100, 200, 300,
];
const CONTRAST_CYCLES_4BPP_WHITE: &[u16; 15] =
    &[10, 10, 8, 8, 8, 8, 8, 10, 10, 15, 15, 20, 20, 100, 300];

/// Display rotation, only 90° increments supported
#[derive(Clone, Copy, Default)]
pub enum DisplayRotation {
    /// No rotation
    #[default]
    Rotate0,
    /// Rotate by 90 degrees clockwise
    Rotate90,
    /// Rotate by 180 degrees clockwise
    Rotate180,
    /// Rotate 270 degrees clockwise
    Rotate270,
}

#[derive(Clone, Copy, Debug)]
pub enum DrawMode {
    BlackOnWhite,
    WhiteOnWhite,
    WhiteOnBlack,
}

#[derive(Clone, Copy, Debug)]
pub struct Rectangle {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl DrawMode {
    fn lut_default(&self) -> u8 {
        match self {
            Self::BlackOnWhite => 0x55,
            Self::WhiteOnBlack | Self::WhiteOnWhite => 0xAA,
        }
    }

    fn contrast_cycles(&self) -> &[u16; 15] {
        match self {
            Self::WhiteOnBlack => CONTRAST_CYCLES_4BPP_WHITE,
            Self::BlackOnWhite | Self::WhiteOnWhite => CONTRAST_CYCLES_4BPP,
        }
    }
}

const TAINTED_ROWS_SIZE: usize = Display::HEIGHT as usize / 8 + 1;
const FRAMEBUFFER_SIZE: usize = (Display::WIDTH / 2) as usize * Display::HEIGHT as usize;
const BYTES_PER_LINE: usize = Display::WIDTH as usize / 4;
const LINE_BYTES_4BPP: usize = Display::WIDTH as usize / 2;
const LINE_BYTES_DIFFERENCE: usize = Display::WIDTH as usize;
const DU_FRAME_TIMES: [u16; 5] = [1000, 1000, 1000, 1000, 1000];
const DU_LUT_PHASE: [[u8; 4]; 16] = [
    [0x15, 0x55, 0x55, 0x55],
    [0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00],
    [0xAA, 0xAA, 0xAA, 0xA8],
];

pub struct Display<'a> {
    epd: ed047tc1::ED047TC1<'a>,
    skipping: u16,
    framebuffer: Box<[u8; FRAMEBUFFER_SIZE]>,
    previous_framebuffer: Box<[u8; FRAMEBUFFER_SIZE]>,
    tainted_rows: [u8; TAINTED_ROWS_SIZE],
    rotation: DisplayRotation,
}

impl<'a> Display<'a> {
    /// Width of the screen.
    pub const WIDTH: u16 = 960;
    /// Height of the screen
    pub const HEIGHT: u16 = 540;
    /// Bounding Box of the screen.
    pub const BOUNDING_BOX: Rectangle = Rectangle {
        x: 0,
        y: 0,
        width: Self::WIDTH,
        height: Self::HEIGHT,
    };
    pub fn new(
        pins: ed047tc1::PinConfig<'a>,
        i2c: peripherals::I2C0<'a>,
        dma: peripherals::DMA_CH0<'a>,
        lcd_cam: peripherals::LCD_CAM<'a>,
        rmt: peripherals::RMT<'a>,
    ) -> Result<Self> {
        Ok(Display {
            epd: ed047tc1::ED047TC1::new(pins, i2c, dma, lcd_cam, rmt)?,
            skipping: 0,
            framebuffer: Box::new([0xFF; FRAMEBUFFER_SIZE]),
            previous_framebuffer: Box::new([0xFF; FRAMEBUFFER_SIZE]),
            tainted_rows: [0; TAINTED_ROWS_SIZE],
            rotation: DisplayRotation::default(),
        })
    }

    /// Set the rotation
    pub fn set_rotation(&mut self, rotation: DisplayRotation) {
        self.rotation = rotation;
    }

    /// Get rotation
    pub fn rotation(&self) -> DisplayRotation {
        self.rotation
    }

    /// Turn the display on.
    pub fn power_on(&mut self) -> Result<()> {
        debug!("Display power on");
        self.epd.power_on()
    }

    /// Turn the display off.
    pub fn power_off(&mut self) -> Result<()> {
        debug!("Display power off");
        self.epd.power_off()
    }

    pub(crate) fn shutdown_inner(mut self) -> Result<()> {
        if let Err(err) = self.power_off() {
            warn!("display power off before shutdown failed: {:?}", err);
        }
        self.epd.shutdown()
    }

    /// Power the display down and enter deep sleep.
    ///
    /// The boot button (`GPIO0`) is always enabled as a wake source. If
    /// `timer` is provided, it is enabled as an additional wake source.
    pub fn deep_sleep(mut self, lpwr: peripherals::LPWR<'a>, timer: Option<Duration>) -> ! {
        if let Err(err) = self.power_off() {
            warn!("display power off before sleep failed: {:?}", err);
        }

        // put the GT911 touch controller to sleep; it lives on the always-on
        // 3.3 V rail and keeps scanning otherwise. its internal sleep state
        // survives the chip's deep sleep (a reset on the next boot wakes it).
        if let Err(err) = self.epd.sleep_touch() {
            warn!("touch sleep before deep sleep failed: {:?}", err);
        }

        // cut the GPS/LoRa 3.3 V rail; left on it draws tens of mA through deep
        // sleep, since the IO expander retains its output state while the chip
        // is asleep.
        if let Err(err) = self.epd.lora_gps_power_off() {
            warn!("lora/gps power off before sleep failed: {:?}", err);
        }

        let boot_button = self.epd.into_boot_button();
        crate::power::deep_sleep(lpwr, boot_button, timer)
    }

    /// Read the panel temperature in degrees Celsius from the TPS65185 PMIC.
    ///
    /// This triggers a one-shot thermistor measurement and blocks until the
    /// conversion completes (~10 ms). The display must be powered on.
    pub fn panel_temperature(&mut self) -> Result<i8> {
        self.epd.panel_temperature()
    }

    /// Read the battery voltage in volts from the on-board BQ27220 fuel gauge.
    pub fn battery_voltage(&mut self) -> Result<f32> {
        Ok(self.epd.battery_voltage_mv()? as f32 / 1000.0)
    }

    /// Read the battery state of charge percentage from the on-board BQ27220
    /// fuel gauge.
    pub fn battery_percentage(&mut self) -> Result<u16> {
        self.epd.battery_state_of_charge()
    }

    /// Return the touchscreen resolution reported by the GT911 controller.
    pub fn touch_resolution(&self) -> (u16, u16) {
        self.epd.touch_resolution()
    }

    /// Read the current input state from the GT911 controller.
    pub fn input(&mut self) -> Result<InputState> {
        self.epd.input_state()
    }

    /// Read the current touch state if the touchscreen is pressed.
    pub fn touch(&mut self) -> Result<Option<TouchState>> {
        Ok(self.input()?.touch)
    }

    /// Return whether the circular home button is currently being reported.
    pub fn home_button(&mut self) -> Result<bool> {
        Ok(self.input()?.buttons.home)
    }

    /// Return whether the board auxiliary button is currently pressed.
    pub fn auxiliary_button(&mut self) -> Result<bool> {
        Ok(self.input()?.buttons.auxiliary)
    }

    /// Return whether the boot button is currently pressed.
    pub fn boot_button(&mut self) -> Result<bool> {
        Ok(self.input()?.buttons.boot)
    }

    /// Sets a single pixel in the framebuffer without updating the display.
    ///
    /// If the provided coordinates are outside the screen, this method returns
    /// [Error::OutOfBounds]. If the provided color is greater than 0x0F,
    /// this method returns [Error::InvalidColor].
    pub fn set_pixel(&mut self, x: u16, y: u16, color: u8) -> Result<()> {
        if x >= Self::WIDTH || y >= Self::HEIGHT {
            return Err(Error::OutOfBounds);
        }
        if color > 0x0F {
            return Err(Error::InvalidColor);
        }
        // Calculate the index in the framebuffer.
        let index: usize = x as usize / 2 + y as usize * (Self::WIDTH as usize / 2);
        let value = self.framebuffer[index];
        if x % 2 == 1 {
            self.framebuffer[index] = (value & 0x0F) | ((color << 4) & 0xF0);
        } else {
            self.framebuffer[index] = (value & 0xF0) | (color & 0x0F);
        }
        // taint row
        let tainted_index = y as usize / 8;
        self.tainted_rows[tainted_index] |= 1 << (y as usize % 8);
        Ok(())
    }

    /// Fill the whole framebuffer with the same color.
    pub fn fill(&mut self, color: u8) -> Result<()> {
        debug!("display fill");
        if color > 0x0F {
            return Err(Error::InvalidColor);
        }
        self.framebuffer.fill(color << 4 | color);
        self.tainted_rows.fill(0xFF);
        Ok(())
    }

    /// Flush updates the display with the contents of the framebuffer. The
    /// method clears the framebuffer. The provided mode should match the
    /// contents of your framebuffer.
    pub fn flush(&mut self, mode: DrawMode) -> Result<()> {
        debug!("display flush");
        self.draw(mode)?;
        self.tainted_rows.fill(0);
        self.previous_framebuffer
            .copy_from_slice(&*self.framebuffer);
        self.framebuffer.fill(0xFF);
        Ok(())
    }

    /// Clears the screen.
    pub fn clear(&mut self) -> Result<()> {
        debug!("display clear");
        self.clear_area(Self::BOUNDING_BOX)
    }

    /// Performs the screen repair routine as described here
    /// https://github.com/Xinyuan-LilyGO/LilyGo-EPD47/blob/master/examples/screen_repair/screen_repair.ino
    pub fn repair(&mut self, delay: Delay) -> Result<()> {
        debug!("display repair");
        self.clear()?;
        for _ in 0..20 {
            self.push_pixels(Self::BOUNDING_BOX, 50, 0)?;
            delay.delay_millis(500);
        }
        self.clear()?;
        for _ in 0..40 {
            self.push_pixels(Self::BOUNDING_BOX, 50, 1)?;
            delay.delay_millis(500);
        }
        self.clear()
    }

    pub fn clear_area(&mut self, area: Rectangle) -> Result<()> {
        let area = self.clip_rectangle(area);
        if area.width == 0 || area.height == 0 {
            return Ok(());
        }
        self.clear_cycles(area, 4, 50)?;
        fill_rect(&mut self.framebuffer, area, 0x0F);
        fill_rect(&mut self.previous_framebuffer, area, 0x0F);
        Ok(())
    }

    /// Fast partial monochrome update using the panel's direct-update waveform.
    ///
    /// This is intended for small text/UI regions where reduced flicker matters
    /// more than perfect grayscale handling.
    pub fn flush_partial_fast(&mut self, area: Rectangle) -> Result<()> {
        let area = self.clip_rectangle(area);
        if area.width == 0 || area.height == 0 {
            return Ok(());
        }

        debug!("display flush partial fast");
        self.draw_partial_du(area)?;
        copy_rect(&mut self.previous_framebuffer, &self.framebuffer, area);
        self.framebuffer.fill(0xFF);
        self.tainted_rows.fill(0);
        Ok(())
    }

    fn clear_cycles(&mut self, area: Rectangle, cycles: u16, cycle_time: u16) -> Result<()> {
        for _ in 0..cycles {
            for _ in 0..4 {
                self.push_pixels(area, cycle_time, 0)?;
            }
            for _ in 0..4 {
                self.push_pixels(area, cycle_time, 1)?;
            }
        }
        Ok(())
    }

    fn draw_partial_du(&mut self, area: Rectangle) -> Result<()> {
        let mut lut = vec![0u8; 1 << 16];
        let mut line = [0u8; LINE_BYTES_DIFFERENCE];

        for output_time in DU_FRAME_TIMES {
            update_du_lut(&mut lut, &DU_LUT_PHASE);
            self.skipping = 0;
            self.epd.frame_start()?;

            for y in 0..Self::HEIGHT {
                if y < area.y || y >= area.y + area.height {
                    self.row_skip(output_time)?;
                    continue;
                }

                let start = y as usize * LINE_BYTES_4BPP;
                let end = start + LINE_BYTES_4BPP;
                let framebuffer_line = &self.framebuffer[start..end];
                let previous_line = &self.previous_framebuffer[start..end];

                if !build_difference_line(framebuffer_line, previous_line, area, &mut line) {
                    self.row_skip(output_time)?;
                    continue;
                }

                let buf = prepare_dma_difference_buffer(&line, &lut);
                self.epd.set_buffer(buf.as_slice())?;
                self.row_write(output_time)?;
            }

            if self.skipping == 0 {
                self.row_write(output_time)?;
            }
            self.epd.frame_end()?;
        }

        Ok(())
    }

    fn clip_rectangle(&self, area: Rectangle) -> Rectangle {
        let x = area.x.min(Self::WIDTH);
        let y = area.y.min(Self::HEIGHT);
        let max_x = area.x.saturating_add(area.width).min(Self::WIDTH);
        let max_y = area.y.saturating_add(area.height).min(Self::HEIGHT);
        Rectangle {
            x,
            y,
            width: max_x.saturating_sub(x),
            height: max_y.saturating_sub(y),
        }
    }

    fn push_pixels(&mut self, area: Rectangle, time: u16, color: u16) -> Result<()> {
        let mut row = [0u8; BYTES_PER_LINE];

        for i in 0..area.width {
            let pos = i + area.x % 4;
            let mask = match color {
                1 => 0b10101010,
                _ => 0b01010101,
            } & (0b00000011 << (2 * (pos % 4)));
            row[(area.x / 4 + pos / 4) as usize] |= mask;
        }
        line_buffer_reorder(&mut row);
        self.epd.frame_start()?;

        for i in 0..Self::HEIGHT {
            // before are of interest: skip
            if i < area.y {
                self.row_skip(time)?;
                continue;
            }
            if i == area.y {
                self.epd.set_buffer(&row)?;
                self.row_write(time)?;
                continue;
            }
            if i >= area.y + area.height {
                self.row_skip(time)?;
                continue;
            }
            self.row_write(time)?;
        }
        self.row_write(time)?;
        self.epd.frame_end()?;

        Ok(())
    }

    fn row_skip(&mut self, output_time: u16) -> Result<()> {
        match self.skipping {
            0 => {
                self.epd.set_buffer(&[0u8; BYTES_PER_LINE])?;
                self.epd.output_row(output_time)?;
            }
            i if i < 2 => {
                self.epd.output_row(10)?;
            }
            _ => {
                self.epd.skip()?;
            }
        }
        self.skipping += 1;

        Ok(())
    }

    fn row_write(&mut self, output_time: u16) -> Result<()> {
        self.skipping = 0;
        self.epd.output_row(output_time)?;

        Ok(())
    }

    fn is_tainted(&self, row: u16) -> bool {
        let index = row as usize / 8;
        self.tainted_rows[index] & (1 << (row as usize % 8)) != 0
    }

    const DRAW_IMAGE_FRAME_COUNT: usize = 15;
    fn draw(&mut self, mode: DrawMode) -> Result<()> {
        // let start = esp_hal::time::current_time();

        // init lut
        let mut lut = vec![mode.lut_default(); 1 << 16];

        for k in 0..Self::DRAW_IMAGE_FRAME_COUNT {
            // update lut
            update_lut(&mut lut, k, mode);
            // start draw
            self.skipping = 0;
            self.epd.frame_start()?;
            // build line
            for y in 0..Self::HEIGHT {
                if !self.is_tainted(y) {
                    self.row_skip(mode.contrast_cycles()[k])?;
                    continue;
                }
                let start = y as usize * LINE_BYTES_4BPP;
                let end = start + LINE_BYTES_4BPP;
                // draw
                let buf = prepare_dma_buffer(&self.framebuffer[start..end], &lut);
                self.epd.set_buffer(buf.as_slice())?;
                self.row_write(mode.contrast_cycles()[k])?;
            }
            if self.skipping == 0 {
                self.row_write(mode.contrast_cycles()[k])?;
            }
            self.epd.frame_end()?;
        }
        // println!(
        //     "draw_fb {}",
        //     (esp_hal::time::current_time() - start).to_millis()
        // );
        Ok(())
    }
}

fn line_buffer_reorder(data: &mut [u8]) {
    // Iterate over the data in chunks of 4 bytes (size of a u32)
    for chunk in data.chunks_exact_mut(4) {
        // Convert the 4-byte chunk to a u32, swap the high and low 16 bits, and then
        // write it back
        let val = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        let swapped = (val >> 16) | ((val & 0x0000FFFF) << 16);
        chunk.copy_from_slice(&swapped.to_le_bytes());
    }
}

fn prepare_dma_buffer(line_data: &[u8], conversion_lut: &[u8]) -> Vec<u8> {
    let mut epd_input = vec![0u8; BYTES_PER_LINE];
    let mut wide_epd_input: Vec<u32> = vec![0u32; Display::WIDTH as usize / 16];

    let line_data_16: Vec<u16> = line_data
        .chunks(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();

    for (j, chunk) in line_data_16.chunks(4).enumerate() {
        if let [v1, v2, v3, v4] = chunk {
            let pixel: u32 = (conversion_lut[*v1 as usize] as u32)
                | (conversion_lut[*v2 as usize] as u32) << 8
                | (conversion_lut[*v3 as usize] as u32) << 16
                | (conversion_lut[*v4 as usize] as u32) << 24;
            wide_epd_input[j] = pixel;
        }
    }

    for (i, &wide_pixel) in wide_epd_input.iter().enumerate() {
        epd_input[i * 4..(i + 1) * 4].copy_from_slice(&wide_pixel.to_le_bytes());
    }

    epd_input
}

fn prepare_dma_difference_buffer(line_data: &[u8], conversion_lut: &[u8]) -> Vec<u8> {
    let mut epd_input = vec![0u8; BYTES_PER_LINE];
    let line_data_16: Vec<u16> = line_data
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();

    for j in 0..BYTES_PER_LINE {
        let v1 = line_data_16[2 * j] as usize;
        let v2 = line_data_16[2 * j + 1] as usize;
        epd_input[j] = conversion_lut[v1] | (conversion_lut[v2] << 4);
    }

    epd_input
}

fn update_lut(conversion_lut: &mut [u8], k: usize, mode: DrawMode) {
    let k = match mode {
        DrawMode::BlackOnWhite | DrawMode::WhiteOnWhite => Display::DRAW_IMAGE_FRAME_COUNT - k,
        DrawMode::WhiteOnBlack => k,
    };
    // reset the pixels which are not to be lightened / darkened
    // any longer in the current frame
    for l in (k..1 << 16).step_by(16) {
        conversion_lut[l] &= 0xFC;
    }
    for l in ((k << 4)..(1 << 16)).step_by(1 << 8) {
        for p in 0..16 {
            conversion_lut[l + p] &= 0xF3
        }
    }
    for l in ((k << 8)..(1 << 16)).step_by(1 << 12) {
        for p in 0..(1 << 8) {
            conversion_lut[l + p] &= 0xCF
        }
    }
    for entry in conversion_lut.iter_mut().take((k + 1) << 12).skip(k << 12) {
        *entry &= 0x3F;
    }
}

fn update_du_lut(conversion_lut: &mut [u8], phase: &[[u8; 4]; 16]) {
    for (to, packed) in phase.iter().enumerate() {
        for (from_packed, value) in packed.iter().enumerate() {
            let index = (to << 4) | (from_packed * 4);
            conversion_lut[index] = (value >> 6) & 0x03;
            conversion_lut[index + 1] = (value >> 4) & 0x03;
            conversion_lut[index + 2] = (value >> 2) & 0x03;
            conversion_lut[index + 3] = value & 0x03;
        }
    }

    for outer in (0..=0xFF).rev() {
        let outer_result = conversion_lut[outer] << 2;
        let base = outer << 8;
        conversion_lut.copy_within(0..0x100, base);
        for entry in &mut conversion_lut[base..base + 0x100] {
            *entry |= outer_result;
        }
    }
}

fn build_difference_line(
    framebuffer_line: &[u8],
    previous_line: &[u8],
    area: Rectangle,
    line: &mut [u8; LINE_BYTES_DIFFERENCE],
) -> bool {
    let mut dirty = false;
    let x_start = area.x as usize;
    let x_end = x_start + area.width as usize;

    for (x, slot) in line.iter_mut().enumerate().take(Display::WIDTH as usize) {
        let previous = nibble_at(previous_line, x);
        let target = if (x_start..x_end).contains(&x) {
            nibble_at(framebuffer_line, x)
        } else {
            previous
        };
        dirty |= target != previous;
        *slot = (target << 4) | previous;
    }

    dirty
}

fn nibble_at(line: &[u8], x: usize) -> u8 {
    let value = line[x / 2];
    if x.is_multiple_of(2) {
        value & 0x0F
    } else {
        value >> 4
    }
}

fn fill_rect(framebuffer: &mut [u8; FRAMEBUFFER_SIZE], area: Rectangle, color: u8) {
    let packed = (color << 4) | color;
    let full_width = area.x == 0 && area.width == Display::WIDTH;
    for y in area.y..area.y + area.height {
        let start = y as usize * LINE_BYTES_4BPP;
        let end = start + LINE_BYTES_4BPP;
        let row = &mut framebuffer[start..end];
        if full_width {
            row.fill(packed);
        } else {
            for x in area.x..area.x + area.width {
                let index = x as usize / 2;
                let value = row[index];
                row[index] = if x % 2 == 0 {
                    (value & 0xF0) | color
                } else {
                    (value & 0x0F) | (color << 4)
                };
            }
        }
    }
}

fn copy_rect(
    destination: &mut [u8; FRAMEBUFFER_SIZE],
    source: &[u8; FRAMEBUFFER_SIZE],
    area: Rectangle,
) {
    for y in area.y..area.y + area.height {
        let row_start = y as usize * LINE_BYTES_4BPP;
        let dest_row = &mut destination[row_start..row_start + LINE_BYTES_4BPP];
        let src_row = &source[row_start..row_start + LINE_BYTES_4BPP];
        for x in area.x..area.x + area.width {
            let index = x as usize / 2;
            if x % 2 == 0 {
                dest_row[index] = (dest_row[index] & 0xF0) | (src_row[index] & 0x0F);
            } else {
                dest_row[index] = (dest_row[index] & 0x0F) | (src_row[index] & 0xF0);
            }
        }
    }
}
