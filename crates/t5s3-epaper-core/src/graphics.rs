use embedded_graphics_core::{pixelcolor::Gray4, prelude::*};

use crate::{
    display::{Display, DisplayRotation},
    Error,
};

impl DrawTarget for Display<'_, '_> {
    type Color = Gray4;

    type Error = Error;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(coord, color) in pixels.into_iter() {
            let Some((x, y)) = translate_coord_rotation(coord.x, coord.y, &self.rotation()) else {
                continue;
            };
            let result = self.set_pixel(x, y, color.luma());
            if matches!(result, Err(Error::OutOfBounds)) {
                continue;
            }
            result?;
        }
        Ok(())
    }

    fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
        self.fill(color.luma())
    }
}

// map a logical coordinate to a native panel coordinate, or None when the
// pixel falls outside the panel. the DrawTarget contract requires drawing to
// ignore out-of-bounds pixels; the previous u16 arithmetic underflowed (and
// panicked with overflow checks on) for coordinates past the rotated edge.
#[inline(always)]
fn translate_coord_rotation(x: i32, y: i32, rotation: &DisplayRotation) -> Option<(u16, u16)> {
    let (width, height) = (i32::from(Display::WIDTH), i32::from(Display::HEIGHT));
    let (nx, ny) = match rotation {
        DisplayRotation::Rotate0 => (x, y),
        DisplayRotation::Rotate90 => (width - 1 - y, x),
        DisplayRotation::Rotate180 => (width - 1 - x, height - 1 - y),
        DisplayRotation::Rotate270 => (y, height - 1 - x),
    };
    if (0..width).contains(&nx) && (0..height).contains(&ny) {
        Some((nx as u16, ny as u16))
    } else {
        None
    }
}

impl OriginDimensions for Display<'_, '_> {
    fn size(&self) -> Size {
        match self.rotation() {
            DisplayRotation::Rotate0 | DisplayRotation::Rotate180 => {
                Size::new(Self::WIDTH as u32, Self::HEIGHT as u32)
            }
            DisplayRotation::Rotate90 | DisplayRotation::Rotate270 => {
                Size::new(Self::HEIGHT as u32, Self::WIDTH as u32)
            }
        }
    }
}

impl From<embedded_graphics_core::primitives::Rectangle> for crate::display::Rectangle {
    fn from(val: embedded_graphics_core::primitives::Rectangle) -> Self {
        crate::display::Rectangle {
            x: val.top_left.x as u16,
            y: val.top_left.y as u16,
            width: val.size.width as u16,
            height: val.size.height as u16,
        }
    }
}
