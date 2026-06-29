use core::fmt::Write as _;

use embedded_graphics::{
    mono_font::{
        ascii::{FONT_6X10, FONT_9X18_BOLD},
        MonoTextStyle,
    },
    prelude::*,
    primitives::{
        Arc,
        Circle,
        Line,
        PrimitiveStyle,
        PrimitiveStyleBuilder,
        Rectangle,
        RoundedRectangle,
        Triangle,
    },
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use t5s3_epaper_core::Display;

use crate::{
    datetime::{DAY_NAMES, MONTH_NAMES},
    fmt::FmtBuf,
    layout::{SCREEN_W, STATUS_H},
    screen::Screen,
};

const COLS: usize = 3;
const ICON_W: u16 = 150;
const ICON_H: u16 = 220;
const GAP_X: u16 = 20;
const GAP_Y: u16 = 40;
const GRID_TOP_Y: i32 = STATUS_H + 40;

pub(crate) struct Icon {
    label: &'static str,
    pub(crate) screen: Screen,
}

pub(crate) const ICONS: [Icon; 6] = [
    Icon {
        label: "GPS",
        screen: Screen::Gps,
    },
    Icon {
        label: "LoRa",
        screen: Screen::Lora,
    },
    Icon {
        label: "Light",
        screen: Screen::Frontlight,
    },
    Icon {
        label: "Sleep",
        screen: Screen::Sleep,
    },
    Icon {
        label: "Files",
        screen: Screen::Files,
    },
    Icon {
        label: "Info",
        screen: Screen::Info,
    },
];

fn grid_origin_x() -> i32 {
    (SCREEN_W - (COLS as i32 * ICON_W as i32 + (COLS as i32 - 1) * GAP_X as i32)) / 2
}

fn icon_rect(idx: usize) -> (i32, i32) {
    let col = idx % COLS;
    let row = idx / COLS;
    (
        grid_origin_x() + (col as i32) * (ICON_W as i32 + GAP_X as i32),
        GRID_TOP_Y + (row as i32) * (ICON_H as i32 + GAP_Y as i32),
    )
}

pub(crate) fn hit_test(sx: i32, sy: i32) -> Option<usize> {
    for i in 0..ICONS.len() {
        let (x, y) = icon_rect(i);
        if sx >= x && sx < x + ICON_W as i32 && sy >= y && sy < y + ICON_H as i32 {
            return Some(i);
        }
    }
    None
}

fn draw_glyph(display: &mut Display, screen: Screen, cx: i32, cy: i32) {
    let fill = PrimitiveStyle::with_fill(Gray4::BLACK);
    let white = PrimitiveStyle::with_fill(Gray4::WHITE);
    let line = PrimitiveStyleBuilder::new()
        .stroke_color(Gray4::BLACK)
        .stroke_width(4)
        .build();
    let outline = PrimitiveStyleBuilder::new()
        .stroke_color(Gray4::BLACK)
        .stroke_width(4)
        .fill_color(Gray4::WHITE)
        .build();

    match screen {
        Screen::Gps => {
            // location pin: solid teardrop with a hole punched out
            Circle::with_center(Point::new(cx, cy - 6), 54)
                .into_styled(fill)
                .draw(display)
                .ok();
            Triangle::new(
                Point::new(cx - 26, cy + 2),
                Point::new(cx + 26, cy + 2),
                Point::new(cx, cy + 48),
            )
            .into_styled(fill)
            .draw(display)
            .ok();
            Circle::with_center(Point::new(cx, cy - 6), 18)
                .into_styled(white)
                .draw(display)
                .ok();
        }
        Screen::Lora => {
            // broadcast: a dot with signal arcs radiating upward
            let base = Point::new(cx, cy);
            Circle::with_center(base, 14)
                .into_styled(fill)
                .draw(display)
                .ok();
            for dia in [38u32, 64, 90] {
                Arc::with_center(base, dia, 50.0_f32.deg(), 80.0_f32.deg())
                    .into_styled(line)
                    .draw(display)
                    .ok();
            }
        }
        Screen::Frontlight => {
            // sun: filled disc with eight rays
            Circle::with_center(Point::new(cx, cy), 34)
                .into_styled(fill)
                .draw(display)
                .ok();
            let rays = [
                (0, -24, 0, -40),
                (0, 24, 0, 40),
                (-24, 0, -40, 0),
                (24, 0, 40, 0),
                (17, -17, 28, -28),
                (-17, -17, -28, -28),
                (17, 17, 28, 28),
                (-17, 17, -28, 28),
            ];
            for (x0, y0, x1, y1) in rays {
                Line::new(Point::new(cx + x0, cy + y0), Point::new(cx + x1, cy + y1))
                    .into_styled(line)
                    .draw(display)
                    .ok();
            }
        }
        Screen::Sleep => {
            // crescent moon: carve a white disc out of a black one
            Circle::with_center(Point::new(cx - 2, cy), 60)
                .into_styled(fill)
                .draw(display)
                .ok();
            Circle::with_center(Point::new(cx + 16, cy - 8), 52)
                .into_styled(white)
                .draw(display)
                .ok();
        }
        Screen::Info => {
            // "i" inside a ring
            Circle::with_center(Point::new(cx, cy), 60)
                .into_styled(outline)
                .draw(display)
                .ok();
            Circle::with_center(Point::new(cx, cy - 16), 9)
                .into_styled(fill)
                .draw(display)
                .ok();
            RoundedRectangle::with_equal_corners(
                Rectangle::new(Point::new(cx - 4, cy - 4), Size::new(8, 26)),
                Size::new(3, 3),
            )
            .into_styled(fill)
            .draw(display)
            .ok();
        }
        Screen::Files => {
            // folder: a tab above an outlined body
            Rectangle::new(Point::new(cx - 34, cy - 22), Size::new(26, 12))
                .into_styled(fill)
                .draw(display)
                .ok();
            RoundedRectangle::with_equal_corners(
                Rectangle::new(Point::new(cx - 36, cy - 12), Size::new(72, 46)),
                Size::new(6, 6),
            )
            .into_styled(outline)
            .draw(display)
            .ok();
        }
        Screen::Home | Screen::Image | Screen::Reader => {}
    }
}

pub(crate) fn draw_home(display: &mut Display, date: Option<(usize, i64, u32, u32)>) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    let small = MonoTextStyle::new(&FONT_6X10, Gray4::new(4));

    // date header above the icon grid, once the clock has been synced
    if let Some((dow, year, month, day)) = date {
        let mut buf = FmtBuf::<32>::new();
        write!(
            buf,
            "{}, {} {}, {}",
            DAY_NAMES[dow],
            MONTH_NAMES[(month - 1) as usize],
            day,
            year
        )
        .ok();
        Text::with_alignment(
            buf.as_str(),
            Point::new(SCREEN_W / 2, 82),
            bold,
            Alignment::Center,
        )
        .draw(display)
        .ok();
    }

    let border = PrimitiveStyleBuilder::new()
        .stroke_color(Gray4::BLACK)
        .stroke_width(2)
        .fill_color(Gray4::WHITE)
        .build();

    for (i, icon) in ICONS.iter().enumerate() {
        let (x, y) = icon_rect(i);

        RoundedRectangle::with_equal_corners(
            Rectangle::new(Point::new(x, y), Size::new(ICON_W as u32, ICON_H as u32)),
            Size::new(16, 16),
        )
        .into_styled(border)
        .draw(display)
        .ok();

        draw_glyph(display, icon.screen, x + ICON_W as i32 / 2, y + 80);

        Text::with_alignment(
            icon.label,
            Point::new(x + ICON_W as i32 / 2, y + ICON_H as i32 - 40),
            small,
            Alignment::Center,
        )
        .draw(display)
        .ok();
    }
}
