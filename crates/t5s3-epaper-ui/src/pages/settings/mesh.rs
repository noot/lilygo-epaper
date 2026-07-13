use embedded_graphics::{
    mono_font::{ascii::FONT_9X15, MonoTextStyle},
    prelude::*,
    primitives::{PrimitiveStyle, PrimitiveStyleBuilder, Rectangle},
    text::Text,
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use t5s3_epaper_core::Display;

use super::{button, in_rect, label, title, BTN_H, WIDE_BTN_W, WIDE_BTN_X};
use crate::{layout::screen_to_native_rect, settings::Settings, widgets::draw_back_button};

const NAME_Y: i32 = 210;
const RADIO_Y: i32 = 300;

// the name editor: a bordered input box above the keyboard, shown while the
// name row is being edited.
const EDIT_X: i32 = 30;
const EDIT_Y: i32 = 420;
const EDIT_W: u32 = 480;
const EDIT_H: u32 = 60;

pub(crate) enum Hit {
    Back,
    EditName,
    ToggleRadio,
}

pub(crate) fn hit_test(sx: i32, sy: i32) -> Option<Hit> {
    if crate::widgets::back_button_hit(sx, sy) {
        Some(Hit::Back)
    } else if in_rect(sx, sy, WIDE_BTN_X, NAME_Y, WIDE_BTN_W, BTN_H) {
        Some(Hit::EditName)
    } else if in_rect(sx, sy, WIDE_BTN_X, RADIO_Y, WIDE_BTN_W, BTN_H) {
        Some(Hit::ToggleRadio)
    } else {
        None
    }
}

pub(crate) fn draw(display: &mut Display, settings: &Settings, editing: bool, draft: &str) {
    draw_back_button(display);
    title(display, "Mesh");

    // node name: the alias flooded to peers, shown next to the node id on
    // their displays. tap to edit with the keyboard.
    label(display, "Node name", NAME_Y);
    draw_name_button(display, settings);

    // lora radio lifetime: page-scoped (default) or always listening in the
    // background at a standing rx current cost.
    label(display, "Mesh radio", RADIO_Y);
    draw_radio_button(display, settings.mesh_background);

    if editing {
        draw_editor(display, draft);
        crate::keyboard::draw(display, false, false, "SAVE");
    }
}

fn draw_name_button(display: &mut Display, settings: &Settings) {
    let name = settings.mesh_alias();
    button(
        display,
        WIDE_BTN_X,
        NAME_Y,
        WIDE_BTN_W,
        if name.is_empty() { "-" } else { name },
    );
}

fn draw_radio_button(display: &mut Display, background: bool) {
    button(
        display,
        WIDE_BTN_X,
        RADIO_Y,
        WIDE_BTN_W,
        if background {
            "Always on"
        } else {
            "Lora page only"
        },
    );
}

pub(crate) fn draw_editor(display: &mut Display, draft: &str) {
    Rectangle::new(Point::new(EDIT_X, EDIT_Y), Size::new(EDIT_W, EDIT_H))
        .into_styled(
            PrimitiveStyleBuilder::new()
                .stroke_color(Gray4::BLACK)
                .stroke_width(2)
                .fill_color(Gray4::WHITE)
                .build(),
        )
        .draw(display)
        .ok();
    let font = MonoTextStyle::new(&FONT_9X15, Gray4::BLACK);
    let mut shown = heapless::String::<16>::new();
    let _ = shown.push_str(draft);
    let _ = shown.push('_');
    Text::new(&shown, Point::new(EDIT_X + 14, EDIT_Y + 36), font)
        .draw(display)
        .ok();
}

pub(crate) fn clear_editor(display: &mut Display) {
    Rectangle::new(Point::new(EDIT_X, EDIT_Y), Size::new(EDIT_W, EDIT_H))
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(display)
        .ok();
}

pub(crate) fn radio_button_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(WIDE_BTN_X, RADIO_Y, WIDE_BTN_W as i32, BTN_H as i32)
}

pub(crate) fn editor_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(EDIT_X, EDIT_Y, EDIT_W as i32, EDIT_H as i32)
}

pub(crate) fn redraw_radio(display: &mut Display, background: bool) {
    draw_radio_button(display, background);
}
