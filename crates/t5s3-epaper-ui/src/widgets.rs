use core::fmt::Write as _;

use embedded_graphics::{
    mono_font::{
        ascii::{FONT_9X15, FONT_9X18_BOLD},
        MonoTextStyle,
    },
    prelude::*,
    primitives::{PrimitiveStyle, PrimitiveStyleBuilder, Rectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use epub_reader::GrayImage;
use t5s3_epaper_core::Display;

use crate::{
    fmt::FmtBuf,
    layout::{screen_to_native_rect, SCREEN_W, STATUS_H},
};

// loop passes between status-bar battery refreshes. the charge changes slowly,
// so a coarse interval keeps the indicator current without frequent partials.
pub(crate) const BATTERY_REFRESH_TICKS: u16 = 600;

fn draw_battery_icon(display: &mut Display, x: i32, y: i32, pct: u16) {
    let body_w: u32 = 30;
    let body_h: u32 = 20;
    let nub_w: u32 = 3;
    let nub_h: u32 = 10;

    // body outline
    Rectangle::new(Point::new(x, y), Size::new(body_w, body_h))
        .into_styled(
            PrimitiveStyleBuilder::new()
                .stroke_color(Gray4::BLACK)
                .stroke_width(2)
                .build(),
        )
        .draw(display)
        .ok();

    // nub
    Rectangle::new(
        Point::new(x + body_w as i32, y + (body_h as i32 - nub_h as i32) / 2),
        Size::new(nub_w, nub_h),
    )
    .into_styled(PrimitiveStyle::with_fill(Gray4::BLACK))
    .draw(display)
    .ok();

    // fill
    let inner_w = body_w - 6;
    let fill_w = inner_w * pct.min(100) as u32 / 100;
    if fill_w > 0 {
        Rectangle::new(Point::new(x + 3, y + 3), Size::new(fill_w, body_h - 6))
            .into_styled(PrimitiveStyle::with_fill(Gray4::new(4)))
            .draw(display)
            .ok();
    }
}

pub(crate) fn draw_status_bar(
    display: &mut Display,
    pct: u16,
    time: Option<(u32, u32)>,
    time_24h: bool,
) {
    draw_statusbar_battery(display, pct);
    draw_statusbar_time(display, time, time_24h);

    Rectangle::new(Point::new(0, STATUS_H - 2), Size::new(SCREEN_W as u32, 2))
        .into_styled(PrimitiveStyle::with_fill(Gray4::new(8)))
        .draw(display)
        .ok();
}

// the clock shown centered in the status bar (24-hour HH:MM, 12-hour h:MM with
// an AM/PM suffix, or --:-- before an NTP sync). drawn over a white fill so the
// once-a-minute partial refresh cleanly replaces the previous value.
pub(crate) fn draw_statusbar_time(display: &mut Display, time: Option<(u32, u32)>, time_24h: bool) {
    Rectangle::new(Point::new(210, 18), Size::new(120, 30))
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(display)
        .ok();
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    let mut buf = FmtBuf::<12>::new();
    match time {
        Some((h, m)) if time_24h => write!(buf, "{h:02}:{m:02}").ok(),
        Some((h, m)) => {
            let suffix = if h < 12 { "AM" } else { "PM" };
            let h12 = match h % 12 {
                0 => 12,
                other => other,
            };
            write!(buf, "{h12}:{m:02} {suffix}").ok()
        }
        None => write!(buf, "--:--").ok(),
    };
    Text::with_alignment(buf.as_str(), Point::new(270, 37), bold, Alignment::Center)
        .draw(display)
        .ok();
}

pub(crate) fn statusbar_time_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(210, 18, 120, 30)
}

// the battery percentage and icon at the right of the status bar. drawn over a
// white fill so the periodic partial refresh cleanly replaces the previous
// value as the charge changes.
pub(crate) fn draw_statusbar_battery(display: &mut Display, pct: u16) {
    Rectangle::new(Point::new(448, 18), Size::new(90, 26))
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(display)
        .ok();
    let status_font = MonoTextStyle::new(&FONT_9X15, Gray4::BLACK);
    let mut buf = FmtBuf::<8>::new();
    write!(buf, "{}%", pct.min(100)).ok();
    Text::with_alignment(
        buf.as_str(),
        Point::new(491, 35),
        status_font,
        Alignment::Right,
    )
    .draw(display)
    .ok();
    draw_battery_icon(display, 497, 20, pct);
}

pub(crate) fn statusbar_battery_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(448, 18, 90, 26)
}

pub(crate) fn draw_back_button(display: &mut Display) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    Text::with_alignment("< Back", Point::new(60, 36), bold, Alignment::Center)
        .draw(display)
        .ok();
}

pub(crate) fn back_button_hit(sx: i32, sy: i32) -> bool {
    sx < 110 && sy < 50
}

// scale `image` to fit inside the box at (bx, by) sized bw x bh, preserving
// aspect ratio and centering it, then blit it with ordered dithering. shared by
// the reader (page images) and the music page (album art).
pub(crate) fn draw_image_fit(
    display: &mut Display,
    image: &GrayImage,
    bx: i32,
    by: i32,
    bw: u32,
    bh: u32,
) {
    let src_w = u32::from(image.width()).max(1);
    let src_h = u32::from(image.height()).max(1);

    let (dst_w, dst_h) = if bw * src_h <= bh * src_w {
        (bw, (bw * src_h / src_w).max(1))
    } else {
        ((bh * src_w / src_h).max(1), bh)
    };

    let off_x = bx + ((bw - dst_w) / 2) as i32;
    let off_y = by + ((bh - dst_h) / 2) as i32;
    let area = Rectangle::new(Point::new(off_x, off_y), Size::new(dst_w, dst_h));

    let colors = (0..dst_h).flat_map(move |dy| {
        (0..dst_w).map(move |dx| {
            let sx = (dx * src_w / dst_w) as u16;
            let sy = (dy * src_h / dst_h) as u16;
            Gray4::new(dither(image.sample(sx, sy), dx, dy))
        })
    });
    display.fill_contiguous(&area, colors).ok();
}

// 4x4 ordered (Bayer) dithering of an 8-bit luma value to a 0..=15 gray level.
fn dither(luma: u8, x: u32, y: u32) -> u8 {
    const BAYER: [[u32; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
    let scaled = u32::from(luma) * 15;
    let base = scaled / 255;
    let frac = scaled % 255;
    let threshold = BAYER[(y & 3) as usize][(x & 3) as usize];
    let level = if frac * 16 / 255 > threshold {
        base + 1
    } else {
        base
    };
    level.min(15) as u8
}
