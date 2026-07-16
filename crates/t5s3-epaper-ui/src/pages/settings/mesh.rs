use t5s3_epaper_core::Display;

use super::{button, in_rect, label, title, BTN_H, WIDE_BTN_W, WIDE_BTN_X};
use crate::{
    layout::screen_to_native_rect,
    settings::Settings,
    text_field::TextField,
    widgets::draw_back_button,
};

const NAME_Y: i32 = 210;
const RADIO_Y: i32 = 300;

// the name editor: a bordered input box above the keyboard, shown while the
// name row is being edited.
pub(crate) const EDIT_X: i32 = 30;
pub(crate) const EDIT_Y: i32 = 420;
pub(crate) const EDIT_W: i32 = 480;
pub(crate) const EDIT_H: i32 = 60;

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

pub(crate) fn draw(
    display: &mut Display,
    settings: &Settings,
    editing: bool,
    draft: &str,
    field: &TextField,
) {
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
        field.draw_full(display, draft);
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

pub(crate) fn radio_button_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(WIDE_BTN_X, RADIO_Y, WIDE_BTN_W as i32, BTN_H as i32)
}

pub(crate) fn redraw_radio(display: &mut Display, background: bool) {
    draw_radio_button(display, background);
}
