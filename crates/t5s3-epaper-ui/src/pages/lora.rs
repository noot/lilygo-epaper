use alloc::{format, string::String, vec::Vec as AVec};

use embedded_graphics::{
    mono_font::{
        ascii::{FONT_6X10, FONT_9X15, FONT_9X18_BOLD},
        MonoTextStyle,
    },
    prelude::*,
    primitives::{Line, PrimitiveStyle, PrimitiveStyleBuilder, Rectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use nootmesh::airtime::Modulation;
use t5s3_epaper_core::{
    lora::{Bandwidth, CodingRate, Config as LoraConfig, Lora, SpreadingFactor},
    spi::Bus,
    Display,
};

use crate::{layout::screen_to_native_rect, widgets::draw_back_button};

/// Which view of the mesh page is open: node/peer info, composing
/// (keyboard), or the scrollable received-message log.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tab {
    Info,
    Send,
    Recv,
}

const TAB_Y: i32 = 64;
const TAB_H: u32 = 48;
const TAB_W: u32 = 150;
const TAB_INFO_X: i32 = 30;
const TAB_SEND_X: i32 = 195;
const TAB_RECV_X: i32 = 360;

const MSG_X: i32 = 30;
const MSG_Y: i32 = 150;
const MSG_W: u32 = 480;
const MSG_H: u32 = 170;
// two-line status (see `Mesh::status_line`), wedged between the message box
// (ends at 320) and the sent list (368).
const LORA_STATUS_Y: i32 = 322;
const STATUS_H: u32 = 44;
pub(crate) const MSG_MAX: usize = 200;

// the send tab's sent-message log, between the status line and the keyboard.
pub(crate) const SENT_Y: i32 = 368;
const LIST_H: u32 = 102;
pub(crate) const LIST_MAX: usize = 3;

// the receive tab: a tall scrollable log (mesh status lives on the info tab).
const RECV_TOP: i32 = 140;
const RECV_ROW_H: i32 = 20;
const RECV_ROWS: i32 = 40;
const RECV_TEXT_X: i32 = 24;
/// wrap width in characters (FONT_6X10 in the text column).
const RECV_CHARS: usize = 70;
const SCROLL_X: i32 = 470;
const SCROLL_W: u32 = 60;
const SCROLL_BTN_H: u32 = 100;
const SCROLL_UP_Y: i32 = RECV_TOP;
const SCROLL_DOWN_Y: i32 = RECV_TOP + (RECV_ROWS - 5) * RECV_ROW_H;
// delete-history button, between the scroll buttons; first tap arms it
// ("SURE?"), the second wipes, any other tap disarms.
const CLEAR_Y: i32 = RECV_TOP + 21 * RECV_ROW_H;
const CLEAR_H: u32 = 60;

/// received messages kept in ram (and as the sd log's retained tail).
pub(crate) const RECV_MAX: usize = 120;

pub(crate) fn tab_hit(sx: i32, sy: i32) -> Option<Tab> {
    if !(TAB_Y..TAB_Y + TAB_H as i32).contains(&sy) {
        return None;
    }
    for (x, tab) in [
        (TAB_INFO_X, Tab::Info),
        (TAB_SEND_X, Tab::Send),
        (TAB_RECV_X, Tab::Recv),
    ] {
        if (x..x + TAB_W as i32).contains(&sx) {
            return Some(tab);
        }
    }
    None
}

pub(crate) fn recv_scroll_up_hit(sx: i32, sy: i32) -> bool {
    (SCROLL_X..SCROLL_X + SCROLL_W as i32).contains(&sx)
        && (SCROLL_UP_Y..SCROLL_UP_Y + SCROLL_BTN_H as i32).contains(&sy)
}

pub(crate) fn recv_scroll_down_hit(sx: i32, sy: i32) -> bool {
    (SCROLL_X..SCROLL_X + SCROLL_W as i32).contains(&sx)
        && (SCROLL_DOWN_Y..SCROLL_DOWN_Y + SCROLL_BTN_H as i32).contains(&sy)
}

pub(crate) fn recv_clear_hit(sx: i32, sy: i32) -> bool {
    (SCROLL_X..SCROLL_X + SCROLL_W as i32).contains(&sx)
        && (CLEAR_Y..CLEAR_Y + CLEAR_H as i32).contains(&sy)
}

/// lines the receive log scrolls over, newest message first: each entry
/// wrapped to at most two rows (a stamped max-length text fits in two),
/// continuation rows indented.
pub(crate) fn recv_lines(entries: &[String]) -> AVec<String> {
    let mut lines = AVec::new();
    for entry in entries.iter().rev() {
        let chars: AVec<char> = entry.chars().collect();
        let first: String = chars.iter().take(RECV_CHARS).collect();
        lines.push(first);
        if chars.len() > RECV_CHARS {
            let rest: String = chars[RECV_CHARS..]
                .iter()
                .take(RECV_CHARS.saturating_sub(2))
                .collect();
            lines.push(format!("  {rest}"));
        }
    }
    lines
}

/// rows the receive log shows at once; scroll moves by a page minus one row.
pub(crate) fn recv_visible_rows() -> usize {
    RECV_ROWS as usize
}

/// the largest useful scroll offset (the view showing the oldest lines).
pub(crate) fn recv_scroll_max(entries: &[String]) -> usize {
    recv_lines(entries).len().saturating_sub(RECV_ROWS as usize)
}

fn draw_tabs(display: &mut Display, tab: Tab) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    let bold_inv = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::WHITE);
    for (label, x, active) in [
        ("Info", TAB_INFO_X, tab == Tab::Info),
        ("Send", TAB_SEND_X, tab == Tab::Send),
        ("Receive", TAB_RECV_X, tab == Tab::Recv),
    ] {
        let style = if active {
            PrimitiveStyle::with_fill(Gray4::BLACK)
        } else {
            PrimitiveStyleBuilder::new()
                .stroke_color(Gray4::BLACK)
                .stroke_width(2)
                .fill_color(Gray4::WHITE)
                .build()
        };
        Rectangle::new(Point::new(x, TAB_Y), Size::new(TAB_W, TAB_H))
            .into_styled(style)
            .draw(display)
            .ok();
        Text::with_alignment(
            label,
            Point::new(x + TAB_W as i32 / 2, TAB_Y + TAB_H as i32 / 2 + 6),
            if active { bold_inv } else { bold },
            Alignment::Center,
        )
        .draw(display)
        .ok();
    }
}

pub(crate) fn draw_message(display: &mut Display, message: &str) {
    Rectangle::new(Point::new(MSG_X, MSG_Y), Size::new(MSG_W, MSG_H))
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
    let x = MSG_X + 12;
    let mut y = MSG_Y + 28;
    if message.is_empty() {
        // hint sits after the cursor, in a lighter shade than typed text
        Text::new(
            " type a message...",
            Point::new(x + 9, y),
            MonoTextStyle::new(&FONT_9X15, Gray4::new(9)),
        )
        .draw(display)
        .ok();
    }

    // trailing cursor makes the input position visible — a just-typed space
    // is otherwise indistinguishable from nothing. wrap on a character count;
    // the font is fixed width and the text is ascii.
    let shown = format!("{message}_");
    let per_line = ((MSG_W as i32 - 24) / 9) as usize;
    let bytes = shown.len();
    let mut start = 0;
    while start < bytes {
        let end = (start + per_line).min(bytes);
        Text::new(&shown[start..end], Point::new(x, y), font)
            .draw(display)
            .ok();
        y += 20;
        start = end;
    }
}

/// the send tab's action feedback line ("queued for our slot", errors).
pub(crate) fn draw_lora_status(display: &mut Display, status: &str) {
    Rectangle::new(Point::new(MSG_X, LORA_STATUS_Y), Size::new(MSG_W, STATUS_H))
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(display)
        .ok();
    Text::with_alignment(
        status,
        Point::new(crate::layout::SCREEN_W / 2, LORA_STATUS_Y + 18),
        MonoTextStyle::new(&FONT_9X15, Gray4::BLACK),
        Alignment::Center,
    )
    .draw(display)
    .ok();
}

// the send tab's sent-message log (newest first), truncated to one line each.
pub(crate) fn draw_list(display: &mut Display, y: i32, header: &str, items: &[String]) {
    Rectangle::new(Point::new(MSG_X, y), Size::new(MSG_W, LIST_H))
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(display)
        .ok();
    Text::new(
        header,
        Point::new(MSG_X + 4, y + 16),
        MonoTextStyle::new(&FONT_9X15, Gray4::BLACK),
    )
    .draw(display)
    .ok();

    let font = MonoTextStyle::new(&FONT_6X10, Gray4::BLACK);
    let mut ey = y + 40;
    for msg in items.iter().rev() {
        // truncate on a char boundary, not a byte index: received messages are
        // arbitrary utf-8 from a peer, so slicing at a fixed byte would panic
        // mid-codepoint.
        let line = match msg.char_indices().nth(66) {
            Some((end, _)) => format!("> {}...", &msg[..end]),
            None => format!("> {msg}"),
        };
        Text::new(&line, Point::new(MSG_X + 10, ey), font)
            .draw(display)
            .ok();
        ey += 20;
    }
}

/// the receive tab's scrollable log: `scroll` is the first visible wrapped
/// line. draws the text column, the scroll buttons and a position hint.
pub(crate) fn draw_recv_list(
    display: &mut Display,
    entries: &[String],
    scroll: usize,
    clear_armed: bool,
) {
    Rectangle::new(
        Point::new(0, RECV_TOP - 4),
        Size::new(540, (RECV_ROWS * RECV_ROW_H + 8) as u32),
    )
    .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
    .draw(display)
    .ok();

    let lines = recv_lines(entries);
    let scroll = scroll.min(lines.len().saturating_sub(1));
    let font = MonoTextStyle::new(&FONT_6X10, Gray4::BLACK);
    if lines.is_empty() {
        Text::new(
            "no messages yet",
            Point::new(RECV_TEXT_X, RECV_TOP + 16),
            font,
        )
        .draw(display)
        .ok();
    }
    let mut y = RECV_TOP + 14;
    for line in lines.iter().skip(scroll).take(RECV_ROWS as usize) {
        Text::new(line, Point::new(RECV_TEXT_X, y), font)
            .draw(display)
            .ok();
        y += RECV_ROW_H;
    }

    // scroll controls, a "shown/total" hint, and the armed-to-confirm
    // history-delete button between them.
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    for (label, by, bh) in [
        ("^", SCROLL_UP_Y, SCROLL_BTN_H),
        ("v", SCROLL_DOWN_Y, SCROLL_BTN_H),
        (if clear_armed { "SURE?" } else { "CLR" }, CLEAR_Y, CLEAR_H),
    ] {
        Rectangle::new(Point::new(SCROLL_X, by), Size::new(SCROLL_W, bh))
            .into_styled(
                PrimitiveStyleBuilder::new()
                    .stroke_color(Gray4::BLACK)
                    .stroke_width(2)
                    .fill_color(if clear_armed && by == CLEAR_Y {
                        Gray4::new(11)
                    } else {
                        Gray4::WHITE
                    })
                    .build(),
            )
            .draw(display)
            .ok();
        let style = if by == CLEAR_Y {
            MonoTextStyle::new(&FONT_6X10, Gray4::BLACK)
        } else {
            bold
        };
        Text::with_alignment(
            label,
            Point::new(SCROLL_X + SCROLL_W as i32 / 2, by + bh as i32 / 2 + 4),
            style,
            Alignment::Center,
        )
        .draw(display)
        .ok();
    }
    let shown = (scroll + RECV_ROWS as usize).min(lines.len());
    Text::with_alignment(
        &format!("{shown}/{}", lines.len()),
        Point::new(
            SCROLL_X + SCROLL_W as i32 / 2,
            RECV_TOP + (RECV_ROWS / 2) * RECV_ROW_H,
        ),
        MonoTextStyle::new(&FONT_6X10, Gray4::BLACK),
        Alignment::Center,
    )
    .draw(display)
    .ok();
}

// the info tab: own node state, then a table of every known peer.
const INFO_TOP: i32 = 140;
const INFO_ROW_H: i32 = 20;
const INFO_ROWS: usize = 22;
const INFO_H: i32 = 134 + INFO_ROWS as i32 * INFO_ROW_H;

pub(crate) fn draw_info_tab(display: &mut Display, mesh: Option<&crate::mesh::Mesh>) {
    Rectangle::new(Point::new(0, INFO_TOP - 4), Size::new(540, INFO_H as u32))
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(display)
        .ok();
    let header = MonoTextStyle::new(&FONT_9X15, Gray4::BLACK);
    let body = MonoTextStyle::new(&FONT_6X10, Gray4::BLACK);
    let Some(mesh) = mesh else {
        Text::new("mesh not running", Point::new(24, INFO_TOP + 18), header)
            .draw(display)
            .ok();
        return;
    };
    for (i, line) in mesh.info_lines().iter().enumerate() {
        Text::new(line, Point::new(24, INFO_TOP + 18 + i as i32 * 24), header)
            .draw(display)
            .ok();
    }
    Line::new(
        Point::new(20, INFO_TOP + 82),
        Point::new(520, INFO_TOP + 82),
    )
    .into_styled(PrimitiveStyle::with_stroke(Gray4::BLACK, 1))
    .draw(display)
    .ok();
    Text::new(
        "peer                  slot   dBm   heard",
        Point::new(24, INFO_TOP + 104),
        body,
    )
    .draw(display)
    .ok();
    let rows = mesh.peer_rows();
    if rows.is_empty() {
        Text::new("no peers heard yet", Point::new(24, INFO_TOP + 128), body)
            .draw(display)
            .ok();
    }
    let mut y = INFO_TOP + 128;
    for row in rows.iter().take(INFO_ROWS) {
        Text::new(row, Point::new(24, y), body).draw(display).ok();
        y += INFO_ROW_H;
    }
}

pub(crate) fn info_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(0, INFO_TOP - 4, 540, INFO_H)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_lora_screen(
    display: &mut Display,
    tab: Tab,
    message: &str,
    status: &str,
    sent: &[String],
    received: &[String],
    scroll: usize,
    symbols: bool,
    shift: bool,
    mesh: Option<&crate::mesh::Mesh>,
) {
    draw_back_button(display);
    draw_tabs(display, tab);
    match tab {
        Tab::Info => draw_info_tab(display, mesh),
        Tab::Send => {
            draw_message(display, message);
            draw_lora_status(display, status);
            draw_list(display, SENT_Y, "sent", sent);
            crate::keyboard::draw(display, symbols, shift, "SEND");
        }
        Tab::Recv => {
            draw_recv_list(display, received, scroll, false);
        }
    }
}

pub(crate) fn message_box_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(MSG_X, MSG_Y, MSG_W as i32, MSG_H as i32)
}

pub(crate) fn lora_status_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(MSG_X, LORA_STATUS_Y, MSG_W as i32, STATUS_H as i32)
}

pub(crate) fn sent_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(MSG_X, SENT_Y, MSG_W as i32, LIST_H as i32)
}

pub(crate) fn recv_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(0, RECV_TOP - 4, 540, RECV_ROWS * RECV_ROW_H + 8)
}

// build the lora radio used by the mesh page. it shares SPI2 with the SD card
// via the bus, which owns and parks both chip-selects. steal the radio's
// control pins (mirroring the wifi re-sync); dropping the returned radio
// releases them. the 3.3v rail powered up at boot, so no settle delay is
// needed.
pub(crate) fn make_radio<'a>(
    bus: &'a Bus<'static>,
) -> Result<Lora<'a, 'static>, t5s3_epaper_core::lora::Error> {
    let pins = t5s3_epaper_core::lora::PinConfig {
        rst: unsafe { esp_hal::peripherals::GPIO1::steal() },
        busy: unsafe { esp_hal::peripherals::GPIO47::steal() },
        dio1: unsafe { esp_hal::peripherals::GPIO10::steal() },
    };
    // derive the modulation from the nootmesh fleet profile, so the radio
    // always matches what the mesh engine's airtime math (and every other
    // node) assumes. this driver's own default is SF10, which the T3-S3
    // nodes cannot demodulate.
    let modulation = Modulation::default();
    let config = LoraConfig {
        spreading_factor: match modulation.spreading_factor() {
            8 => SpreadingFactor::Sf8,
            9 => SpreadingFactor::Sf9,
            10 => SpreadingFactor::Sf10,
            11 => SpreadingFactor::Sf11,
            12 => SpreadingFactor::Sf12,
            _ => SpreadingFactor::Sf7,
        },
        bandwidth: match modulation.bandwidth_hz() {
            250_000 => Bandwidth::Bw250,
            500_000 => Bandwidth::Bw500,
            _ => Bandwidth::Bw125,
        },
        coding_rate: match modulation.coding_rate_denominator() {
            6 => CodingRate::Cr4_6,
            7 => CodingRate::Cr4_7,
            8 => CodingRate::Cr4_8,
            _ => CodingRate::Cr4_5,
        },
        preamble_length: modulation.preamble_symbols(),
        // match the t3-s3 relays' +22 dBm (this driver defaults to 17):
        // asymmetric power made walker->home links die ~5 dB before
        // home->walker, observed as "peers alive but my sends lost"
        tx_power_dbm: 22,
        ..LoraConfig::default()
    };
    Lora::new(bus, pins, &config)
}
