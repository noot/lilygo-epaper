use core::fmt::Write as _;

use embedded_graphics::{
    mono_font::{
        ascii::{FONT_9X15, FONT_9X18_BOLD},
        MonoTextStyle,
    },
    prelude::*,
    primitives::{PrimitiveStyleBuilder, Rectangle, RoundedRectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use t5s3_epaper_core::Display;

use crate::{
    fmt::FmtBuf,
    layout::{screen_to_native_rect, SCREEN_W},
    settings::Settings,
    widgets::draw_back_button,
};

const LABEL_X: i32 = 40;
const BTN_H: u32 = 64;
const SMALL_BTN_W: u32 = 60;
const WIDE_BTN_X: i32 = 280;
const WIDE_BTN_W: u32 = 220;

const TZ_Y: i32 = 210;
const TZ_MINUS_X: i32 = 280;
const TZ_PLUS_X: i32 = 440;
const TZ_VAL_X: i32 = 345;
const TZ_VAL_W: u32 = 90;

const FMT_Y: i32 = 300;
const ICONS_Y: i32 = 380;
const ICON_SIZE_Y: i32 = 460;
const READER_HDR_Y: i32 = 545;
const FONT_SIZE_Y: i32 = 585;
const FONT_FAMILY_Y: i32 = 675;
const SPACING_Y: i32 = 765;

// which control a tap landed on, dispatched by the main loop.
pub(crate) enum Hit {
    Back,
    TzMinus,
    TzPlus,
    ToggleFormat,
    CycleIcons,
    CycleIconSize,
    CycleFontSize,
    CycleFontFamily,
    CycleSpacing,
}

fn in_rect(sx: i32, sy: i32, x: i32, y: i32, w: u32, h: u32) -> bool {
    (x..x + w as i32).contains(&sx) && (y..y + h as i32).contains(&sy)
}

pub(crate) fn hit_test(sx: i32, sy: i32) -> Option<Hit> {
    if crate::widgets::back_button_hit(sx, sy) {
        Some(Hit::Back)
    } else if in_rect(sx, sy, TZ_MINUS_X, TZ_Y, SMALL_BTN_W, BTN_H) {
        Some(Hit::TzMinus)
    } else if in_rect(sx, sy, TZ_PLUS_X, TZ_Y, SMALL_BTN_W, BTN_H) {
        Some(Hit::TzPlus)
    } else if in_rect(sx, sy, WIDE_BTN_X, FMT_Y, WIDE_BTN_W, BTN_H) {
        Some(Hit::ToggleFormat)
    } else if in_rect(sx, sy, WIDE_BTN_X, ICONS_Y, WIDE_BTN_W, BTN_H) {
        Some(Hit::CycleIcons)
    } else if in_rect(sx, sy, WIDE_BTN_X, ICON_SIZE_Y, WIDE_BTN_W, BTN_H) {
        Some(Hit::CycleIconSize)
    } else if in_rect(sx, sy, WIDE_BTN_X, FONT_SIZE_Y, WIDE_BTN_W, BTN_H) {
        Some(Hit::CycleFontSize)
    } else if in_rect(sx, sy, WIDE_BTN_X, FONT_FAMILY_Y, WIDE_BTN_W, BTN_H) {
        Some(Hit::CycleFontFamily)
    } else if in_rect(sx, sy, WIDE_BTN_X, SPACING_Y, WIDE_BTN_W, BTN_H) {
        Some(Hit::CycleSpacing)
    } else {
        None
    }
}

fn button(display: &mut Display, x: i32, y: i32, w: u32, label: &str) {
    let border = PrimitiveStyleBuilder::new()
        .stroke_color(Gray4::BLACK)
        .stroke_width(3)
        .fill_color(Gray4::WHITE)
        .build();
    RoundedRectangle::with_equal_corners(
        Rectangle::new(Point::new(x, y), Size::new(w, BTN_H)),
        Size::new(10, 10),
    )
    .into_styled(border)
    .draw(display)
    .ok();
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    Text::with_alignment(
        label,
        Point::new(x + w as i32 / 2, y + BTN_H as i32 / 2 + 6),
        bold,
        Alignment::Center,
    )
    .draw(display)
    .ok();
}

fn label(display: &mut Display, text: &str, row_y: i32) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    Text::with_alignment(
        text,
        Point::new(LABEL_X, row_y + BTN_H as i32 / 2 + 6),
        bold,
        Alignment::Left,
    )
    .draw(display)
    .ok();
}

pub(crate) fn draw_settings_screen(display: &mut Display, settings: &Settings) {
    draw_back_button(display);

    Text::with_alignment(
        "Settings",
        Point::new(SCREEN_W / 2, 120),
        MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK),
        Alignment::Center,
    )
    .draw(display)
    .ok();

    // timezone row: a -/+ stepper around the current offset.
    label(display, "Timezone", TZ_Y);
    button(display, TZ_MINUS_X, TZ_Y, SMALL_BTN_W, "-");
    button(display, TZ_PLUS_X, TZ_Y, SMALL_BTN_W, "+");
    draw_tz_value(display, settings.tz_offset_hours);

    // time-format row: tap the value to toggle between 12- and 24-hour.
    label(display, "Time format", FMT_Y);
    draw_format_button(display, settings.time_24h);

    // home-screen icon set and size.
    label(display, "Icons", ICONS_Y);
    draw_icons_button(display, settings);

    label(display, "Icon size", ICON_SIZE_Y);
    draw_icon_size_button(display, settings);

    // reader section.
    Text::with_alignment(
        "Reader",
        Point::new(LABEL_X, READER_HDR_Y),
        MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::new(4)),
        Alignment::Left,
    )
    .draw(display)
    .ok();
    Rectangle::new(
        Point::new(LABEL_X, READER_HDR_Y + 10),
        Size::new((SCREEN_W - 2 * LABEL_X) as u32, 2),
    )
    .into_styled(embedded_graphics::primitives::PrimitiveStyle::with_fill(
        Gray4::new(8),
    ))
    .draw(display)
    .ok();

    label(display, "Font size", FONT_SIZE_Y);
    draw_font_size_button(display, settings);

    label(display, "Font", FONT_FAMILY_Y);
    draw_family_button(display, settings);

    label(display, "Spacing", SPACING_Y);
    draw_spacing_button(display, settings);
}

fn draw_tz_value(display: &mut Display, offset_hours: i8) {
    Rectangle::new(Point::new(TZ_VAL_X, TZ_Y), Size::new(TZ_VAL_W, BTN_H))
        .into_styled(embedded_graphics::primitives::PrimitiveStyle::with_fill(
            Gray4::WHITE,
        ))
        .draw(display)
        .ok();
    let mut buf = FmtBuf::<12>::new();
    write!(buf, "UTC{offset_hours:+}").ok();
    Text::with_alignment(
        buf.as_str(),
        Point::new(TZ_VAL_X + TZ_VAL_W as i32 / 2, TZ_Y + BTN_H as i32 / 2 + 5),
        MonoTextStyle::new(&FONT_9X15, Gray4::BLACK),
        Alignment::Center,
    )
    .draw(display)
    .ok();
}

fn draw_format_button(display: &mut Display, time_24h: bool) {
    button(
        display,
        WIDE_BTN_X,
        FMT_Y,
        WIDE_BTN_W,
        if time_24h { "24-hour" } else { "12-hour" },
    );
}

fn draw_icons_button(display: &mut Display, settings: &Settings) {
    button(
        display,
        WIDE_BTN_X,
        ICONS_Y,
        WIDE_BTN_W,
        settings.icon_style.label(),
    );
}

fn draw_icon_size_button(display: &mut Display, settings: &Settings) {
    button(
        display,
        WIDE_BTN_X,
        ICON_SIZE_Y,
        WIDE_BTN_W,
        settings.icon_size.label(),
    );
}

fn draw_font_size_button(display: &mut Display, settings: &Settings) {
    button(
        display,
        WIDE_BTN_X,
        FONT_SIZE_Y,
        WIDE_BTN_W,
        settings.reader_font_size.label(),
    );
}

fn draw_family_button(display: &mut Display, settings: &Settings) {
    button(
        display,
        WIDE_BTN_X,
        FONT_FAMILY_Y,
        WIDE_BTN_W,
        settings.reader_font_family.label(),
    );
}

fn draw_spacing_button(display: &mut Display, settings: &Settings) {
    button(
        display,
        WIDE_BTN_X,
        SPACING_Y,
        WIDE_BTN_W,
        settings.reader_line_spacing.label(),
    );
}

pub(crate) fn tz_value_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(TZ_VAL_X, TZ_Y, TZ_VAL_W as i32, BTN_H as i32)
}

pub(crate) fn format_button_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(WIDE_BTN_X, FMT_Y, WIDE_BTN_W as i32, BTN_H as i32)
}

pub(crate) fn icons_button_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(WIDE_BTN_X, ICONS_Y, WIDE_BTN_W as i32, BTN_H as i32)
}

pub(crate) fn icon_size_button_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(WIDE_BTN_X, ICON_SIZE_Y, WIDE_BTN_W as i32, BTN_H as i32)
}

pub(crate) fn font_size_button_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(WIDE_BTN_X, FONT_SIZE_Y, WIDE_BTN_W as i32, BTN_H as i32)
}

pub(crate) fn family_button_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(WIDE_BTN_X, FONT_FAMILY_Y, WIDE_BTN_W as i32, BTN_H as i32)
}

pub(crate) fn spacing_button_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(WIDE_BTN_X, SPACING_Y, WIDE_BTN_W as i32, BTN_H as i32)
}

pub(crate) fn redraw_tz(display: &mut Display, offset_hours: i8) {
    draw_tz_value(display, offset_hours);
}

pub(crate) fn redraw_format(display: &mut Display, time_24h: bool) {
    draw_format_button(display, time_24h);
}

pub(crate) fn redraw_icons(display: &mut Display, settings: &Settings) {
    draw_icons_button(display, settings);
}

pub(crate) fn redraw_icon_size(display: &mut Display, settings: &Settings) {
    draw_icon_size_button(display, settings);
}

pub(crate) fn redraw_font_size(display: &mut Display, settings: &Settings) {
    draw_font_size_button(display, settings);
}

pub(crate) fn redraw_family(display: &mut Display, settings: &Settings) {
    draw_family_button(display, settings);
}

pub(crate) fn redraw_spacing(display: &mut Display, settings: &Settings) {
    draw_spacing_button(display, settings);
}
