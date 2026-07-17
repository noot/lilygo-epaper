use core::fmt::Write as _;

use embedded_graphics::{
    mono_font::{ascii::FONT_9X15, MonoTextStyle},
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use t5s3_epaper_core::Display;

use super::{button, in_rect, label, title, BTN_H, WIDE_BTN_W, WIDE_BTN_X};
use crate::{
    fmt::FmtBuf,
    layout::screen_to_native_rect,
    settings::Settings,
    widgets::draw_back_button,
};

const SMALL_BTN_W: u32 = 60;

const TZ_Y: i32 = 210;
const TZ_MINUS_X: i32 = 280;
const TZ_PLUS_X: i32 = 440;
const TZ_VAL_X: i32 = 345;
const TZ_VAL_W: u32 = 90;

const FMT_Y: i32 = 300;
const ICONS_Y: i32 = 390;
const ICON_SIZE_Y: i32 = 480;
const IO48_Y: i32 = 570;

pub(crate) enum Hit {
    Back,
    TzMinus,
    TzPlus,
    ToggleFormat,
    CycleIcons,
    CycleIconSize,
    CycleIo48,
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
    } else if in_rect(sx, sy, WIDE_BTN_X, IO48_Y, WIDE_BTN_W, BTN_H) {
        Some(Hit::CycleIo48)
    } else {
        None
    }
}

pub(crate) fn draw(display: &mut Display, settings: &Settings) {
    draw_back_button(display);
    title(display, "System");

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

    // IO48 button action selection.
    label(display, "IO48 Button", IO48_Y);
    draw_io48_button(display, settings);
}

fn draw_tz_value(display: &mut Display, offset_hours: i8) {
    Rectangle::new(Point::new(TZ_VAL_X, TZ_Y), Size::new(TZ_VAL_W, BTN_H))
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
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

fn draw_io48_button(display: &mut Display, settings: &Settings) {
    button(
        display,
        WIDE_BTN_X,
        IO48_Y,
        WIDE_BTN_W,
        settings.io48_action.label(),
    );
}

pub(crate) fn io48_button_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(WIDE_BTN_X, IO48_Y, WIDE_BTN_W as i32, BTN_H as i32)
}

pub(crate) fn redraw_io48(display: &mut Display, settings: &Settings) {
    draw_io48_button(display, settings);
}
