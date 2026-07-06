use alloc::{
    borrow::Cow,
    format,
    string::{String, ToString as _},
    vec::Vec,
};
use core::cell::RefCell;

use embedded_graphics::{
    mono_font::{
        ascii::{FONT_9X15, FONT_9X18_BOLD},
        MonoTextStyle,
    },
    prelude::*,
    primitives::{PrimitiveStyle, PrimitiveStyleBuilder, Rectangle, RoundedRectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use esp_hal::{
    gpio::{Level, Output, OutputConfig},
    spi::master::Spi,
    Blocking,
};
use t5s3_epaper_core::{sdcard::Error, Display, SdCard};

use crate::{
    keyboard,
    layout::{screen_to_native_rect, SCREEN_W},
    widgets::draw_back_button,
};

// notes live as NOTE####.TXT files under /NOTES: embedded_sdmmc cannot create
// long filenames, so both the folder and the generated names stay 8.3-safe.
const NOTES_DIR: &str = "/NOTES";
// typing stops at this many bytes; notes grown larger off-device still open
// and save in full, they just cannot grow further on-device.
pub(crate) const NOTE_MAX: usize = 8192;

const TITLE_Y: i32 = 95;
const LIST_X: i32 = 20;
const LIST_W: i32 = 500;
const LIST_TOP: i32 = 150;
const ROW_H: i32 = 42;
pub(crate) const VISIBLE: usize = 14;
const LIST_H: i32 = ROW_H * VISIBLE as i32;
const FOOTER_Y: i32 = LIST_TOP + LIST_H + 22;
const BTN_Y: i32 = FOOTER_Y + 14;
const BTN_W: u32 = 150;
const BTN_H: u32 = 80;
const UP_BTN_X: i32 = 25;
const DOWN_BTN_X: i32 = 195;
const NEW_BTN_X: i32 = 365;
const PREVIEW_CHARS: usize = 40;

// the delete button shares the editor's title row: the status bar's battery
// indicator occupies the screen's top-right corner, so the row below it is
// the free spot opposite the centered note name.
const DELETE_X: i32 = 410;
const DELETE_Y: i32 = 66;
const DELETE_W: i32 = 120;
const DELETE_H: i32 = 30;

// the editor's text area sits between the status bar and the keyboard.
const EDIT_X: i32 = 20;
const EDIT_Y: i32 = 100;
const EDIT_W: i32 = 500;
const EDIT_H: i32 = 490;
const EDIT_LINE_H: i32 = 20;
// FONT_9X15 is 9px wide; the area is padded 12px on each side.
const EDIT_COLS: usize = ((EDIT_W - 24) / 9) as usize;
const EDIT_LINES: usize = ((EDIT_H - 30) / EDIT_LINE_H) as usize;

pub(crate) struct Entry {
    pub(crate) name: String,
    preview: String,
}

// mount the SD card on `bus`, mirroring the file browser: hold the LoRa
// chip-select high to release MISO for the duration. the returned guard and
// card must not outlive `bus`.
fn mount<'a>(
    bus: &'a RefCell<Spi<'static, Blocking>>,
) -> Result<(Output<'static>, SdCard<'a, 'static>), Error> {
    let lora_cs = Output::new(
        unsafe { esp_hal::peripherals::GPIO46::steal() },
        Level::High,
        OutputConfig::default(),
    );
    let card = SdCard::new(unsafe { esp_hal::peripherals::GPIO12::steal() }, bus)?;
    Ok((lora_cs, card))
}

fn note_path(name: &str) -> String {
    format!("{NOTES_DIR}/{name}")
}

fn is_txt(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("txt"))
}

// the first non-blank line of a note, shown as its row in the list.
fn preview(text: &str) -> Option<String> {
    let line = text.lines().find(|l| !l.trim().is_empty())?;
    Some(line.trim_end().chars().take(PREVIEW_CHARS).collect())
}

// list the notes folder newest-first (names are numbered monotonically), with
// each note's first line as its preview. a missing folder is an empty list,
// not an error, so the page works before the first note is ever saved.
pub(crate) fn load_list(bus: &RefCell<Spi<'static, Blocking>>) -> Result<Vec<Entry>, Error> {
    let (_lora_cs, card) = mount(bus)?;
    let entries = match card.exists(NOTES_DIR)? {
        true => card.list_dir(NOTES_DIR)?,
        false => Vec::new(),
    };
    let mut notes = Vec::new();
    for entry in entries {
        if entry.is_directory || !is_txt(&entry.name) {
            continue;
        }
        let preview = card
            .read_file(&entry.path)
            .ok()
            .and_then(|bytes| preview(&String::from_utf8_lossy(&bytes)))
            .unwrap_or_else(|| entry.name.clone());
        notes.push(Entry {
            name: entry.name,
            preview,
        });
    }
    notes.sort_by(|a, b| b.name.cmp(&a.name));
    Ok(notes)
}

fn note_number(name: &str) -> Option<u32> {
    name.strip_prefix("NOTE")?
        .strip_suffix(".TXT")?
        .parse()
        .ok()
}

// the filename for a new note: one past the highest existing number, falling
// back to the first free slot once 9999 is reached. None only when all 9999
// slots are taken.
pub(crate) fn next_name(entries: &[Entry]) -> Option<String> {
    let max = entries
        .iter()
        .filter_map(|e| note_number(&e.name))
        .max()
        .unwrap_or(0);
    if max < 9999 {
        return Some(format!("NOTE{:04}.TXT", max + 1));
    }
    let used: alloc::collections::BTreeSet<u32> = entries
        .iter()
        .filter_map(|e| note_number(&e.name))
        .collect();
    (1..=9999u32)
        .find(|n| !used.contains(n))
        .map(|n| format!("NOTE{n:04}.TXT"))
}

pub(crate) fn load_note(
    bus: &RefCell<Spi<'static, Blocking>>,
    name: &str,
) -> Result<String, Error> {
    let (_lora_cs, card) = mount(bus)?;
    let bytes = card.read_file(&note_path(name))?;
    Ok(match String::from_utf8_lossy(&bytes) {
        Cow::Borrowed(s) => s.to_string(),
        Cow::Owned(s) => s,
    })
}

// write the note's text to its file, creating the folder on first use. an
// empty buffer for a note that was never written is skipped, so backing out
// of an untouched new note leaves no empty file behind.
pub(crate) fn save(bus: &RefCell<Spi<'static, Blocking>>, name: &str, text: &str) {
    let (_lora_cs, card) = match mount(bus) {
        Ok(mounted) => mounted,
        Err(e) => {
            esp_println::println!("notes: save mount failed: {e:?}");
            return;
        }
    };
    let path = note_path(name);
    if text.is_empty() && !card.exists(&path).unwrap_or(false) {
        return;
    }
    card.create_dir_all(NOTES_DIR).ok();
    if let Err(e) = card.write_file(&path, text.as_bytes()) {
        esp_println::println!("notes: save {name} failed: {e:?}");
    }
}

// remove the note's file from the card. a note that was never saved has no
// file yet, so deleting it just discards the buffer.
pub(crate) fn delete(bus: &RefCell<Spi<'static, Blocking>>, name: &str) {
    let (_lora_cs, card) = match mount(bus) {
        Ok(mounted) => mounted,
        Err(e) => {
            esp_println::println!("notes: delete mount failed: {e:?}");
            return;
        }
    };
    let path = note_path(name);
    if !card.exists(&path).unwrap_or(false) {
        return;
    }
    if let Err(e) = card.delete_file(&path) {
        esp_println::println!("notes: delete {name} failed: {e:?}");
    }
}

pub(crate) fn list_hit(sx: i32, sy: i32, count: usize, scroll: usize) -> Option<usize> {
    if !(LIST_X..LIST_X + LIST_W).contains(&sx) || !(LIST_TOP..LIST_TOP + LIST_H).contains(&sy) {
        return None;
    }
    let slot = ((sy - LIST_TOP) / ROW_H) as usize;
    let i = scroll + slot;
    (slot < VISIBLE && i < count).then_some(i)
}

fn button_hit(sx: i32, sy: i32, x: i32) -> bool {
    (x..x + BTN_W as i32).contains(&sx) && (BTN_Y..BTN_Y + BTN_H as i32).contains(&sy)
}

pub(crate) fn scroll_up_hit(sx: i32, sy: i32) -> bool {
    button_hit(sx, sy, UP_BTN_X)
}

pub(crate) fn scroll_down_hit(sx: i32, sy: i32) -> bool {
    button_hit(sx, sy, DOWN_BTN_X)
}

pub(crate) fn new_hit(sx: i32, sy: i32) -> bool {
    button_hit(sx, sy, NEW_BTN_X)
}

pub(crate) fn delete_hit(sx: i32, sy: i32) -> bool {
    sx >= DELETE_X && (55..DELETE_Y + DELETE_H).contains(&sy)
}

// the editor's delete button: deleting is destructive, so the first tap only
// arms it ("Sure?") and the second deletes; any other tap disarms it.
pub(crate) fn draw_delete_button(display: &mut Display, armed: bool) {
    Rectangle::new(
        Point::new(DELETE_X, DELETE_Y),
        Size::new(DELETE_W as u32, DELETE_H as u32),
    )
    .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
    .draw(display)
    .ok();
    Text::with_alignment(
        if armed { "Sure?" } else { "Delete" },
        Point::new(DELETE_X + DELETE_W, 88),
        MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK),
        Alignment::Right,
    )
    .draw(display)
    .ok();
}

pub(crate) fn delete_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(DELETE_X, DELETE_Y, DELETE_W, DELETE_H)
}

fn draw_button(display: &mut Display, x: i32, label: &str) {
    let border = PrimitiveStyleBuilder::new()
        .stroke_color(Gray4::BLACK)
        .stroke_width(2)
        .fill_color(Gray4::WHITE)
        .build();
    RoundedRectangle::with_equal_corners(
        Rectangle::new(Point::new(x, BTN_Y), Size::new(BTN_W, BTN_H)),
        Size::new(10, 10),
    )
    .into_styled(border)
    .draw(display)
    .ok();
    Text::with_alignment(
        label,
        Point::new(x + BTN_W as i32 / 2, BTN_Y + BTN_H as i32 / 2 + 6),
        MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK),
        Alignment::Center,
    )
    .draw(display)
    .ok();
}

pub(crate) fn draw_note_list(display: &mut Display, entries: &[Entry], scroll: usize) {
    Rectangle::new(
        Point::new(LIST_X, LIST_TOP),
        Size::new(LIST_W as u32, LIST_H as u32),
    )
    .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
    .draw(display)
    .ok();

    let font = MonoTextStyle::new(&FONT_9X15, Gray4::BLACK);
    if entries.is_empty() {
        Text::with_alignment(
            "no notes yet - tap New",
            Point::new(SCREEN_W / 2, LIST_TOP + 60),
            MonoTextStyle::new(&FONT_9X15, Gray4::new(4)),
            Alignment::Center,
        )
        .draw(display)
        .ok();
        return;
    }
    for (slot, entry) in entries.iter().skip(scroll).take(VISIBLE).enumerate() {
        let y = LIST_TOP + slot as i32 * ROW_H + 28;
        Text::new(&entry.preview, Point::new(LIST_X + 8, y), font)
            .draw(display)
            .ok();
    }
}

pub(crate) fn draw_notes_footer(display: &mut Display, status: &str) {
    Rectangle::new(
        Point::new(LIST_X, FOOTER_Y - 16),
        Size::new(LIST_W as u32, 24),
    )
    .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
    .draw(display)
    .ok();
    Text::new(
        status,
        Point::new(LIST_X, FOOTER_Y),
        MonoTextStyle::new(&FONT_9X15, Gray4::BLACK),
    )
    .draw(display)
    .ok();
}

pub(crate) fn draw_list_screen(
    display: &mut Display,
    entries: &[Entry],
    scroll: usize,
    status: &str,
) {
    draw_back_button(display);
    Text::with_alignment(
        "Notes",
        Point::new(SCREEN_W / 2, TITLE_Y),
        MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK),
        Alignment::Center,
    )
    .draw(display)
    .ok();
    draw_note_list(display, entries, scroll);
    draw_notes_footer(display, status);
    draw_button(display, UP_BTN_X, "Up");
    draw_button(display, DOWN_BTN_X, "Down");
    draw_button(display, NEW_BTN_X, "New");
}

pub(crate) fn list_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(LIST_X, LIST_TOP, LIST_W, LIST_H)
}

pub(crate) fn footer_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(LIST_X, FOOTER_Y - 16, LIST_W, 24)
}

// wrap `text` into byte ranges of at most EDIT_COLS characters per line,
// breaking at newlines and, when a line overflows, at its last space. the
// final (possibly empty) line is always emitted so the cursor has a home.
fn wrap_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut count = 0;
    let mut last_space = None;
    for (i, c) in text.char_indices() {
        if c == '\n' {
            lines.push((start, i));
            start = i + 1;
            count = 0;
            last_space = None;
            continue;
        }
        if count == EDIT_COLS {
            match last_space {
                Some(s) => {
                    lines.push((start, s));
                    start = s + 1;
                }
                None => {
                    lines.push((start, i));
                    start = i;
                }
            }
            count = text[start..i].chars().count();
            last_space = None;
        }
        if c == ' ' {
            last_space = Some(i);
        }
        count += 1;
    }
    lines.push((start, text.len()));
    lines
}

// the editor's text area: the wrapped text's last screenful with a cursor bar
// at the end, so typing always edits in view.
pub(crate) fn draw_note_text(display: &mut Display, text: &str) {
    Rectangle::new(
        Point::new(EDIT_X, EDIT_Y),
        Size::new(EDIT_W as u32, EDIT_H as u32),
    )
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
    let lines = wrap_ranges(text);
    let first = lines.len().saturating_sub(EDIT_LINES);
    let mut y = EDIT_Y + 26;
    let mut cursor = Point::new(EDIT_X + 12, y);
    for &(a, b) in &lines[first..] {
        Text::new(&text[a..b], Point::new(EDIT_X + 12, y), font)
            .draw(display)
            .ok();
        cursor = Point::new(EDIT_X + 12 + text[a..b].chars().count() as i32 * 9, y);
        y += EDIT_LINE_H;
    }
    Rectangle::new(Point::new(cursor.x + 1, cursor.y - 12), Size::new(2, 15))
        .into_styled(PrimitiveStyle::with_fill(Gray4::BLACK))
        .draw(display)
        .ok();
    if text.is_empty() {
        Text::new(
            "type a note...",
            Point::new(cursor.x + 8, cursor.y),
            MonoTextStyle::new(&FONT_9X15, Gray4::new(4)),
        )
        .draw(display)
        .ok();
    }
}

pub(crate) fn draw_edit_screen(
    display: &mut Display,
    name: &str,
    text: &str,
    symbols: bool,
    shift: bool,
    delete_armed: bool,
) {
    draw_back_button(display);
    let title = name.rsplit_once('.').map_or(name, |(base, _)| base);
    Text::with_alignment(
        title,
        Point::new(SCREEN_W / 2, 88),
        MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK),
        Alignment::Center,
    )
    .draw(display)
    .ok();
    draw_delete_button(display, delete_armed);
    draw_note_text(display, text);
    keyboard::draw(display, symbols, shift, "RET");
}

pub(crate) fn text_area_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(EDIT_X, EDIT_Y, EDIT_W, EDIT_H)
}
