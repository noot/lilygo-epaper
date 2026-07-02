use alloc::{format, string::String, vec::Vec};
use core::cell::RefCell;

use embedded_graphics::{
    mono_font::{
        ascii::{FONT_6X10, FONT_9X15, FONT_9X18_BOLD},
        MonoTextStyle,
    },
    prelude::*,
    primitives::{PrimitiveStyle, PrimitiveStyleBuilder, Rectangle, RoundedRectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use epub_reader::{chapter_number, decode_image, Epub, GrayImage};
use esp_hal::{
    gpio::{Level, Output, OutputConfig},
    spi::master::Spi,
    Blocking,
};
use t5s3_epaper_core::{sdcard::Error, Display, SdCard};

use crate::{
    layout::SCREEN_W,
    pages::reader::{content_key, read_progress, Progress},
    widgets::{draw_back_button, draw_image_fit},
};

// where per-book metadata and cover thumbnails are cached, alongside the
// reader's `.POS` bookmarks. 8.3-safe: an <=8 char dir plus files named from a
// 32-bit key (8 hex chars) with a 3-char extension.
const CACHE_DIR: &str = "/READER";

// bounds on the scan so a large or deeply-nested card can't stall the UI or
// exhaust memory with decoded thumbnails.
const MAX_BOOKS: usize = 48;
const MAX_DEPTH: usize = 6;

// cover thumbnail box (also the cached thumbnail's maximum dimensions).
const THUMB_W: u16 = 104;
const THUMB_H: u16 = 152;

const CARD_X: i32 = 20;
const CARD_W: i32 = 500;
const CARD_TOP: i32 = 115;
const CARD_H: i32 = 168;
const CARD_STRIDE: i32 = 176;
pub(crate) const VISIBLE: usize = 4;

const COVER_X: i32 = CARD_X + 8;
const COVER_OFF_Y: i32 = 8;
const TEXT_X: i32 = CARD_X + THUMB_W as i32 + 24;

const FOOTER_Y: i32 = 838;
const SCROLL_Y: i32 = 855;
const SCROLL_BTN_W: u32 = 200;
const SCROLL_BTN_H: u32 = 64;
const UP_BTN_X: i32 = 40;
const DOWN_BTN_X: i32 = 300;

// one book on the shelf: its location, metadata, decoded cover thumbnail, and
// current reading position (read live from its bookmark so it stays accurate).
pub(crate) struct Book {
    path: String,
    title: String,
    author: String,
    chapter_count: usize,
    // spine indices that begin a real chapter per the TOC (see the epub-reader
    // crate); empty when the book has no usable TOC.
    nav_starts: Vec<usize>,
    // current reading position from the bookmark: chapter (spine index), the page
    // within it, and that section's page count (0 if unknown), used together for
    // a page-refined overall-progress bar.
    chapter: usize,
    page: usize,
    page_count: usize,
    started: bool,
    cover: Option<GrayImage>,
}

impl Book {
    pub(crate) fn path(&self) -> &str {
        &self.path
    }
}

// what the library page is currently showing.
pub(crate) enum View {
    // shown while the (possibly slow first-run) scan is in flight.
    Loading,
    Ready(Vec<Book>),
}

pub(crate) fn is_epub(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("epub"))
}

// scan the card for epubs, resolve each one's metadata + cover thumbnail (from
// the cache when possible), read its bookmark, and return the shelf sorted with
// in-progress books first. self-contained mount, mirroring the file browser.
pub(crate) fn load_library() -> View {
    let bus = match sd_bus() {
        Ok(bus) => bus,
        Err(e) => {
            esp_println::println!("library: sd init failed: {e:?}");
            return View::Ready(Vec::new());
        }
    };
    let (_lora_cs, card) = match mount(&bus) {
        Ok(pair) => pair,
        Err(e) => {
            esp_println::println!("library: sd init failed: {e:?}");
            return View::Ready(Vec::new());
        }
    };

    let mut found: Vec<(String, u32)> = Vec::new();
    scan(&card, &mut found);

    card.create_dir_all(CACHE_DIR).ok();
    let mut books: Vec<Book> = Vec::new();
    for (path, size) in found {
        if let Some(book) = resolve_book(&card, &path, size) {
            books.push(book);
        }
    }

    // in-progress books first, then alphabetical by title.
    books.sort_by(|a, b| {
        b.started
            .cmp(&a.started)
            .then_with(|| a.title.as_str().cmp(b.title.as_str()))
    });
    View::Ready(books)
}

// depth-bounded walk collecting (path, size) for every `.epub`, skipping the
// cache directory and dotfiles, stopping once `MAX_BOOKS` are found.
fn scan(card: &SdCard, out: &mut Vec<(String, u32)>) {
    let mut stack: Vec<(String, usize)> = alloc::vec![(String::from("/"), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if out.len() >= MAX_BOOKS {
            return;
        }
        let entries = match card.list_dir(&dir) {
            Ok(entries) => entries,
            Err(e) => {
                esp_println::println!("library: list {dir} failed: {e:?}");
                continue;
            }
        };
        for entry in entries {
            if entry.name.starts_with('.') {
                continue;
            }
            if entry.is_directory {
                if depth < MAX_DEPTH && entry.path != CACHE_DIR {
                    stack.push((entry.path, depth + 1));
                }
            } else if is_epub(&entry.name) {
                out.push((entry.path, entry.size));
                if out.len() >= MAX_BOOKS {
                    return;
                }
            }
        }
    }
}

// build a `Book` for one epub, reading its metadata + thumbnail from the cache
// when a matching entry exists, otherwise parsing the epub once and populating
// the cache. `None` only if the file can't be read at all.
fn resolve_book(card: &SdCard, path: &str, size: u32) -> Option<Book> {
    let ck = cache_key(path, size);
    let meta = match read_cache(card, ck) {
        Some(meta) => meta,
        None => build_cache(card, ck, path)?,
    };

    let Progress {
        chapter,
        page,
        page_count,
    } = read_progress(card, meta.key);
    Some(Book {
        path: String::from(path),
        title: meta.title,
        author: meta.author,
        chapter_count: meta.chapter_count,
        nav_starts: meta.nav_starts,
        chapter,
        page,
        page_count,
        started: chapter > 0 || page > 0,
        cover: meta.cover,
    })
}

// tag on the metadata file's first line; bumped whenever the format changes so
// stale caches are treated as a miss and rebuilt.
const CACHE_VERSION: &str = "v2";

// a book's cached metadata: the content-hash key (for its bookmark), display
// fields, real-chapter starts, and decoded cover thumbnail.
struct CachedMeta {
    key: u32,
    title: String,
    author: String,
    chapter_count: usize,
    nav_starts: Vec<usize>,
    cover: Option<GrayImage>,
}

// read a cached metadata (`.LIB`) + thumbnail (`.THM`) pair for `ck`. both must
// be present, current-version, and parseable, otherwise the caller rebuilds.
fn read_cache(card: &SdCard, ck: u32) -> Option<CachedMeta> {
    let meta = card.read_file(&meta_path(ck)).ok()?;
    let text = core::str::from_utf8(&meta).ok()?;
    let mut lines = text.split('\n');
    let mut header = lines.next()?.split_whitespace();
    if header.next()? != CACHE_VERSION {
        return None;
    }
    let key = u32::from_str_radix(header.next()?, 16).ok()?;
    let chapter_count = header.next()?.parse().ok()?;
    let title = String::from(lines.next().unwrap_or(""));
    let author = String::from(lines.next().unwrap_or(""));
    let nav_starts = lines
        .next()
        .unwrap_or("")
        .split_whitespace()
        .filter_map(|t| t.parse().ok())
        .collect();

    let thumb = card.read_file(&thumb_path(ck)).ok()?;
    let cover = decode_thumb(&thumb);
    Some(CachedMeta {
        key,
        title,
        author,
        chapter_count,
        nav_starts,
        cover,
    })
}

// parse the epub once to extract its metadata + cover, cache both, and return
// the same `CachedMeta` `read_cache` would.
fn build_cache(card: &SdCard, ck: u32, path: &str) -> Option<CachedMeta> {
    let bytes = match card.read_file(path) {
        Ok(bytes) => bytes,
        Err(e) => {
            esp_println::println!("library: read {path} failed: {e:?}");
            return None;
        }
    };
    let key = content_key(&bytes);
    let epub = match Epub::open(bytes) {
        Ok(epub) => epub,
        Err(e) => {
            esp_println::println!("library: open {path} failed: {e}");
            return None;
        }
    };
    let title = String::from(epub.meta().title().unwrap_or("Untitled"));
    let author = String::from(epub.meta().author().unwrap_or(""));
    let chapter_count = epub.chapter_count().max(1);
    let nav_starts = epub.nav_starts().to_vec();

    let cover = epub
        .cover()
        .ok()
        .and_then(|raw| decode_image(&raw).ok())
        .and_then(|image| downscale(&image, THUMB_W, THUMB_H));

    let nav = nav_starts
        .iter()
        .map(|start| format!("{start}"))
        .collect::<Vec<_>>()
        .join(" ");
    let meta = format!("{CACHE_VERSION} {key:08X} {chapter_count}\n{title}\n{author}\n{nav}\n");
    if let Err(e) = card.write_file(&meta_path(ck), meta.as_bytes()) {
        esp_println::println!("library: cache meta {path} failed: {e:?}");
    }
    if let Err(e) = card.write_file(&thumb_path(ck), &encode_thumb(cover.as_ref())) {
        esp_println::println!("library: cache thumb {path} failed: {e:?}");
    }

    Some(CachedMeta {
        key,
        title,
        author,
        chapter_count,
        nav_starts,
        cover,
    })
}

// nearest-neighbour downscale of a decoded cover to fit within (max_w, max_h),
// preserving aspect ratio, so only a small thumbnail is cached and kept
// resident.
fn downscale(src: &GrayImage, max_w: u16, max_h: u16) -> Option<GrayImage> {
    let sw = u32::from(src.width()).max(1);
    let sh = u32::from(src.height()).max(1);
    let (mw, mh) = (u32::from(max_w), u32::from(max_h));
    let (dw, dh) = if mw * sh <= mh * sw {
        (mw, (mw * sh / sw).max(1))
    } else {
        ((mh * sw / sh).max(1), mh)
    };
    let mut pixels = Vec::with_capacity((dw * dh) as usize);
    for dy in 0..dh {
        for dx in 0..dw {
            pixels.push(src.sample((dx * sw / dw) as u16, (dy * sh / dh) as u16));
        }
    }
    GrayImage::from_raw(dw as u16, dh as u16, pixels).ok()
}

// a thumbnail on disk: little-endian width, height, then row-major luma. a 0x0
// image marks "checked, no cover" so a coverless book isn't re-parsed on every
// scan.
fn encode_thumb(cover: Option<&GrayImage>) -> Vec<u8> {
    let (w, h, pixels): (u16, u16, &[u8]) = match cover {
        Some(image) if image.width() > 0 && image.height() > 0 => {
            (image.width(), image.height(), image.pixels())
        }
        _ => (0, 0, &[]),
    };
    let mut out = Vec::with_capacity(4 + pixels.len());
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(pixels);
    out
}

fn decode_thumb(bytes: &[u8]) -> Option<GrayImage> {
    if bytes.len() < 4 {
        return None;
    }
    let w = u16::from_le_bytes([bytes[0], bytes[1]]);
    let h = u16::from_le_bytes([bytes[2], bytes[3]]);
    if w == 0 || h == 0 {
        return None;
    }
    let pixels = bytes.get(4..4 + w as usize * h as usize)?;
    GrayImage::from_raw(w, h, pixels.to_vec()).ok()
}

pub(crate) fn draw_screen(display: &mut Display, view: &View, scroll: usize) {
    draw_back_button(display);
    Text::with_alignment(
        "Library",
        Point::new(SCREEN_W / 2, 95),
        MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK),
        Alignment::Center,
    )
    .draw(display)
    .ok();

    match view {
        View::Loading => centered(display, "scanning SD card for books...", 460),
        View::Ready(books) if books.is_empty() => {
            centered(display, "no epubs found on SD card", 460);
        }
        View::Ready(books) => {
            for slot in 0..VISIBLE {
                let Some(book) = books.get(scroll + slot) else {
                    break;
                };
                draw_card(display, book, CARD_TOP + slot as i32 * CARD_STRIDE);
            }
            let mut footer = format!("{} books", books.len());
            let pages = books.len().div_ceil(VISIBLE);
            if pages > 1 {
                footer = format!("{footer}    page {} / {}", scroll / VISIBLE + 1, pages);
            }
            Text::with_alignment(
                &footer,
                Point::new(SCREEN_W / 2, FOOTER_Y),
                MonoTextStyle::new(&FONT_9X15, Gray4::new(4)),
                Alignment::Center,
            )
            .draw(display)
            .ok();
            draw_button(display, UP_BTN_X, "Up");
            draw_button(display, DOWN_BTN_X, "Down");
        }
    }
}

fn draw_card(display: &mut Display, book: &Book, card_y: i32) {
    RoundedRectangle::with_equal_corners(
        Rectangle::new(
            Point::new(CARD_X, card_y),
            Size::new(CARD_W as u32, CARD_H as u32),
        ),
        Size::new(12, 12),
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

    draw_cover(display, book.cover.as_ref(), card_y + COVER_OFF_Y);

    let text_w = CARD_X + CARD_W - TEXT_X - 8;
    Text::new(
        &truncate(&book.title, text_w, 9),
        Point::new(TEXT_X, card_y + 42),
        MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK),
    )
    .draw(display)
    .ok();
    if !book.author.is_empty() {
        Text::new(
            &truncate(&book.author, text_w, 9),
            Point::new(TEXT_X, card_y + 72),
            MonoTextStyle::new(&FONT_9X15, Gray4::new(4)),
        )
        .draw(display)
        .ok();
    }

    draw_progress(display, book, card_y);
}

// a labelled progress bar. the label is the chapter (numbered against the TOC
// when available, front matter shown as such; a percentage for TOC-less
// size-split books). the bar tracks overall reading progress through the whole
// book, refined by the page within the current section. unopened books show a
// plain "Not started" with no bar.
fn draw_progress(display: &mut Display, book: &Book, card_y: i32) {
    let label_style = MonoTextStyle::new(&FONT_9X15, Gray4::new(4));
    let bar_y = card_y + CARD_H - 40;
    let bar_w = (CARD_X + CARD_W - TEXT_X - 8) as u32;

    if !book.started {
        Text::new("Not started", Point::new(TEXT_X, bar_y), label_style)
            .draw(display)
            .ok();
        return;
    }

    let permille = overall_permille(book);
    let label = match chapter_number(&book.nav_starts, book.chapter) {
        Some((0, _)) => String::from("Front matter"),
        Some((chapter, total)) => format!("Chapter {chapter} of {total}"),
        None => format!("{}% read", permille / 10),
    };

    Text::new(&label, Point::new(TEXT_X, bar_y - 12), label_style)
        .draw(display)
        .ok();
    Rectangle::new(Point::new(TEXT_X, bar_y), Size::new(bar_w, 14))
        .into_styled(
            PrimitiveStyleBuilder::new()
                .stroke_color(Gray4::new(6))
                .stroke_width(1)
                .build(),
        )
        .draw(display)
        .ok();
    let fill = bar_w * permille / 1000;
    if fill > 0 {
        Rectangle::new(Point::new(TEXT_X, bar_y), Size::new(fill, 14))
            .into_styled(PrimitiveStyle::with_fill(Gray4::new(4)))
            .draw(display)
            .ok();
    }
}

// overall reading progress through the whole book in per-mille (0..=1000): the
// current section's position among all spine items, refined by the page within
// it. treats each spine item as equal weight (page counts vary per section, and
// pre-computing exact byte weights isn't worth the cost); older bookmarks with
// no page count fall back to whole-section granularity.
fn overall_permille(book: &Book) -> u32 {
    let sections = book.chapter_count.max(1) as u32;
    let within = if book.page_count > 0 {
        (book.page as u32 * 1000 / book.page_count as u32).min(1000)
    } else {
        0
    };
    ((book.chapter as u32 * 1000 + within) / sections).min(1000)
}

fn draw_cover(display: &mut Display, cover: Option<&GrayImage>, cover_y: i32) {
    match cover {
        Some(image) => draw_image_fit(
            display,
            image,
            COVER_X,
            cover_y,
            u32::from(THUMB_W),
            u32::from(THUMB_H),
        ),
        None => {
            Text::with_alignment(
                "no cover",
                Point::new(COVER_X + THUMB_W as i32 / 2, cover_y + THUMB_H as i32 / 2),
                MonoTextStyle::new(&FONT_6X10, Gray4::new(8)),
                Alignment::Center,
            )
            .draw(display)
            .ok();
        }
    }
    Rectangle::new(
        Point::new(COVER_X, cover_y),
        Size::new(u32::from(THUMB_W), u32::from(THUMB_H)),
    )
    .into_styled(
        PrimitiveStyleBuilder::new()
            .stroke_color(Gray4::new(8))
            .stroke_width(1)
            .build(),
    )
    .draw(display)
    .ok();
}

fn draw_button(display: &mut Display, x: i32, label: &str) {
    RoundedRectangle::with_equal_corners(
        Rectangle::new(
            Point::new(x, SCROLL_Y),
            Size::new(SCROLL_BTN_W, SCROLL_BTN_H),
        ),
        Size::new(10, 10),
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
    Text::with_alignment(
        label,
        Point::new(
            x + SCROLL_BTN_W as i32 / 2,
            SCROLL_Y + SCROLL_BTN_H as i32 / 2 + 6,
        ),
        MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK),
        Alignment::Center,
    )
    .draw(display)
    .ok();
}

fn centered(display: &mut Display, text: &str, y: i32) {
    Text::with_alignment(
        text,
        Point::new(SCREEN_W / 2, y),
        MonoTextStyle::new(&FONT_9X15, Gray4::new(4)),
        Alignment::Center,
    )
    .draw(display)
    .ok();
}

// which shelf card, if any, a screen-space tap landed on, as an index into the
// book list.
pub(crate) fn card_hit(sx: i32, sy: i32, scroll: usize, count: usize) -> Option<usize> {
    if !(CARD_X..CARD_X + CARD_W).contains(&sx) {
        return None;
    }
    for slot in 0..VISIBLE {
        let card_y = CARD_TOP + slot as i32 * CARD_STRIDE;
        if (card_y..card_y + CARD_H).contains(&sy) {
            let idx = scroll + slot;
            return (idx < count).then_some(idx);
        }
    }
    None
}

pub(crate) fn scroll_up_hit(sx: i32, sy: i32) -> bool {
    (UP_BTN_X..UP_BTN_X + SCROLL_BTN_W as i32).contains(&sx)
        && (SCROLL_Y..SCROLL_Y + SCROLL_BTN_H as i32).contains(&sy)
}

pub(crate) fn scroll_down_hit(sx: i32, sy: i32) -> bool {
    (DOWN_BTN_X..DOWN_BTN_X + SCROLL_BTN_W as i32).contains(&sx)
        && (SCROLL_Y..SCROLL_Y + SCROLL_BTN_H as i32).contains(&sy)
}

// truncate `s` to fit `max_w` pixels for a fixed-advance font of `char_w`,
// appending an ellipsis when clipped.
fn truncate(s: &str, max_w: i32, char_w: i32) -> String {
    let max_chars = (max_w / char_w).max(1) as usize;
    if s.chars().count() <= max_chars {
        return String::from(s);
    }
    let head: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{head}\u{2026}")
}

fn meta_path(ck: u32) -> String {
    format!("{CACHE_DIR}/{ck:08X}.LIB")
}

fn thumb_path(ck: u32) -> String {
    format!("{CACHE_DIR}/{ck:08X}.THM")
}

// cache key from path + size (no file read), so an unchanged book skips the
// expensive parse-and-decode on subsequent scans.
fn cache_key(path: &str, size: u32) -> u32 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |b: u8| {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for &b in path.as_bytes() {
        mix(b);
    }
    for b in size.to_le_bytes() {
        mix(b);
    }
    hash as u32
}

// build the shared spi bus for card access. kept as a local by the caller so it
// outlives the SdCard borrowed from it.
fn sd_bus() -> Result<RefCell<Spi<'static, Blocking>>, Error> {
    t5s3_epaper_core::sdcard::shared_bus(
        unsafe { esp_hal::peripherals::SPI2::steal() },
        unsafe { esp_hal::peripherals::GPIO14::steal() },
        unsafe { esp_hal::peripherals::GPIO13::steal() },
        unsafe { esp_hal::peripherals::GPIO21::steal() },
    )
}

// mount the SD card on `bus`, mirroring the file browser/reader: hold the LoRa
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
