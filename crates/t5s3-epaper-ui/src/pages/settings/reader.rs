use t5s3_epaper_core::Display;

use super::{button, in_rect, label, title, BTN_H, WIDE_BTN_W, WIDE_BTN_X};
use crate::{layout::screen_to_native_rect, settings::Settings, widgets::draw_back_button};

const FONT_SIZE_Y: i32 = 210;
const FONT_FAMILY_Y: i32 = 300;
const SPACING_Y: i32 = 390;

pub(crate) enum Hit {
    Back,
    CycleFontSize,
    CycleFontFamily,
    CycleSpacing,
}

pub(crate) fn hit_test(sx: i32, sy: i32) -> Option<Hit> {
    if crate::widgets::back_button_hit(sx, sy) {
        Some(Hit::Back)
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

pub(crate) fn draw(display: &mut Display, settings: &Settings) {
    draw_back_button(display);
    title(display, "Reader");

    label(display, "Font size", FONT_SIZE_Y);
    draw_font_size_button(display, settings);

    label(display, "Font", FONT_FAMILY_Y);
    draw_family_button(display, settings);

    label(display, "Spacing", SPACING_Y);
    draw_spacing_button(display, settings);
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

pub(crate) fn font_size_button_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(WIDE_BTN_X, FONT_SIZE_Y, WIDE_BTN_W as i32, BTN_H as i32)
}

pub(crate) fn family_button_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(WIDE_BTN_X, FONT_FAMILY_Y, WIDE_BTN_W as i32, BTN_H as i32)
}

pub(crate) fn spacing_button_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(WIDE_BTN_X, SPACING_Y, WIDE_BTN_W as i32, BTN_H as i32)
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
