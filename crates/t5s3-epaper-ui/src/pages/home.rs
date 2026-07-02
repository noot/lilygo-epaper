use core::fmt::Write as _;

use embedded_graphics::{
    image::Image,
    mono_font::{
        ascii::{FONT_6X10, FONT_9X18_BOLD},
        MonoTextStyle,
    },
    prelude::*,
    primitives::{PrimitiveStyleBuilder, Rectangle, RoundedRectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use t5s3_epaper_core::Display;
use tinybmp::Bmp;

use crate::{
    datetime::{DAY_NAMES, MONTH_NAMES},
    fmt::FmtBuf,
    layout::{SCREEN_W, STATUS_H},
    screen::Screen,
    settings::{IconSize, IconStyle},
};

const COLS: usize = 3;
const ICON_W: u16 = 150;
const ICON_H: u16 = 190;
const GAP_X: u16 = 20;
const GAP_Y: u16 = 24;
const GRID_TOP_Y: i32 = STATUS_H + 35;

pub(crate) struct Icon {
    label: &'static str,
    pub(crate) screen: Screen,
}

pub(crate) const ICONS: [Icon; 11] = [
    Icon {
        label: "GPS",
        screen: Screen::Gps,
    },
    Icon {
        label: "LoRa",
        screen: Screen::Lora,
    },
    Icon {
        label: "Library",
        screen: Screen::Library,
    },
    Icon {
        label: "Music",
        screen: Screen::Music,
    },
    Icon {
        label: "Environment",
        screen: Screen::Environment,
    },
    Icon {
        label: "Weather",
        screen: Screen::Weather,
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
        label: "Settings",
        screen: Screen::Settings,
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

macro_rules! icon_set {
    ($dir:literal, $screen:expr) => {
        match $screen {
            Screen::Gps => include_bytes!(concat!("../../assets/icons/", $dir, "/gps.bmp")),
            Screen::Lora => include_bytes!(concat!("../../assets/icons/", $dir, "/lora.bmp")),
            Screen::Frontlight => {
                include_bytes!(concat!("../../assets/icons/", $dir, "/light.bmp"))
            }
            Screen::Sleep => include_bytes!(concat!("../../assets/icons/", $dir, "/sleep.bmp")),
            Screen::Files => include_bytes!(concat!("../../assets/icons/", $dir, "/files.bmp")),
            Screen::Info => include_bytes!(concat!("../../assets/icons/", $dir, "/info.bmp")),
            Screen::Settings => {
                include_bytes!(concat!("../../assets/icons/", $dir, "/settings.bmp"))
            }
            Screen::Music => include_bytes!(concat!("../../assets/icons/", $dir, "/music.bmp")),
            Screen::Environment => {
                include_bytes!(concat!("../../assets/icons/", $dir, "/environment.bmp"))
            }
            Screen::Weather => {
                include_bytes!(concat!("../../assets/icons/", $dir, "/weather.bmp"))
            }
            Screen::Library => include_bytes!(concat!("../../assets/icons/", $dir, "/reader.bmp")),
            Screen::Home | Screen::Image | Screen::Reader => return None,
        }
    };
}

fn icon_bytes(screen: Screen, style: IconStyle, size: IconSize) -> Option<&'static [u8]> {
    Some(match (style, size) {
        (IconStyle::Lucide, IconSize::Regular) => icon_set!("lucide/regular", screen),
        (IconStyle::Lucide, IconSize::Small) => icon_set!("lucide/small", screen),
        (IconStyle::Material, IconSize::Regular) => icon_set!("material/regular", screen),
        (IconStyle::Material, IconSize::Small) => icon_set!("material/small", screen),
    })
}

fn draw_glyph(
    display: &mut Display,
    screen: Screen,
    style: IconStyle,
    size: IconSize,
    cx: i32,
    cy: i32,
) {
    let Some(bytes) = icon_bytes(screen, style, size) else {
        return;
    };
    let Ok(bmp) = Bmp::<Gray4>::from_slice(bytes) else {
        return;
    };
    let dim = bmp.size();
    let top_left = Point::new(cx - dim.width as i32 / 2, cy - dim.height as i32 / 2);
    Image::new(&bmp, top_left).draw(display).ok();
}

pub(crate) fn draw_home(
    display: &mut Display,
    date: Option<(usize, i64, u32, u32)>,
    icon_style: IconStyle,
    icon_size: IconSize,
) {
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

        draw_glyph(
            display,
            icon.screen,
            icon_style,
            icon_size,
            x + ICON_W as i32 / 2,
            y + 80,
        );

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
