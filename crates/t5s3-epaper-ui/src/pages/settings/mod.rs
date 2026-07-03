pub(crate) mod reader;
pub(crate) mod system;
pub(crate) mod wifi;

use embedded_graphics::{
    mono_font::{ascii::FONT_9X18_BOLD, MonoTextStyle},
    prelude::*,
    primitives::{PrimitiveStyleBuilder, Rectangle, RoundedRectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use t5s3_epaper_core::Display;

use crate::{layout::SCREEN_W, widgets::draw_back_button};

// shared geometry for the per-setting rows on the sub-pages: a label at the
// left and a control button at the right of each row.
pub(super) const LABEL_X: i32 = 40;
pub(super) const BTN_H: u32 = 64;
pub(super) const WIDE_BTN_X: i32 = 280;
pub(super) const WIDE_BTN_W: u32 = 220;

pub(super) fn in_rect(sx: i32, sy: i32, x: i32, y: i32, w: u32, h: u32) -> bool {
    (x..x + w as i32).contains(&sx) && (y..y + h as i32).contains(&sy)
}

// a bordered, labelled control button, shared by the settings sub-pages.
pub(super) fn button(display: &mut Display, x: i32, y: i32, w: u32, text: &str) {
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
        text,
        Point::new(x + w as i32 / 2, y + BTN_H as i32 / 2 + 6),
        bold,
        Alignment::Center,
    )
    .draw(display)
    .ok();
}

// the left-aligned label for a settings row.
pub(super) fn label(display: &mut Display, text: &str, row_y: i32) {
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

// a centered screen title just below the status bar.
pub(super) fn title(display: &mut Display, text: &str) {
    Text::with_alignment(
        text,
        Point::new(SCREEN_W / 2, 120),
        MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK),
        Alignment::Center,
    )
    .draw(display)
    .ok();
}

// the settings menu: three entries opening the system, reader, and wifi
// sub-pages. tapping an entry navigates into its screen; Back returns Home.
const MENU_X: i32 = 40;
const MENU_W: u32 = 460;
const MENU_H: u32 = 90;
const MENU_TOP: i32 = 210;
const MENU_GAP: i32 = 30;

pub(crate) enum MenuHit {
    Back,
    System,
    Reader,
    Wifi,
}

const MENU_ENTRIES: [&str; 3] = ["System", "Reader", "Wi-Fi"];

fn menu_row_y(i: i32) -> i32 {
    MENU_TOP + i * (MENU_H as i32 + MENU_GAP)
}

pub(crate) fn menu_hit(sx: i32, sy: i32) -> Option<MenuHit> {
    if crate::widgets::back_button_hit(sx, sy) {
        return Some(MenuHit::Back);
    }
    for i in 0..MENU_ENTRIES.len() as i32 {
        if in_rect(sx, sy, MENU_X, menu_row_y(i), MENU_W, MENU_H) {
            return Some(match i {
                0 => MenuHit::System,
                1 => MenuHit::Reader,
                _ => MenuHit::Wifi,
            });
        }
    }
    None
}

pub(crate) fn draw_menu(display: &mut Display) {
    draw_back_button(display);
    title(display, "Settings");

    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    for (i, entry) in MENU_ENTRIES.iter().enumerate() {
        let y = menu_row_y(i as i32);
        RoundedRectangle::with_equal_corners(
            Rectangle::new(Point::new(MENU_X, y), Size::new(MENU_W, MENU_H)),
            Size::new(12, 12),
        )
        .into_styled(
            PrimitiveStyleBuilder::new()
                .stroke_color(Gray4::BLACK)
                .stroke_width(3)
                .fill_color(Gray4::WHITE)
                .build(),
        )
        .draw(display)
        .ok();
        Text::with_alignment(
            entry,
            Point::new(MENU_X + 24, y + MENU_H as i32 / 2 + 6),
            bold,
            Alignment::Left,
        )
        .draw(display)
        .ok();
        Text::with_alignment(
            ">",
            Point::new(MENU_X + MENU_W as i32 - 24, y + MENU_H as i32 / 2 + 6),
            bold,
            Alignment::Right,
        )
        .draw(display)
        .ok();
    }
}
