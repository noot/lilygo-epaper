use alloc::{format, string::String, vec, vec::Vec};

use embedded_graphics::{
    mono_font::{ascii::FONT_9X15, MonoTextStyle},
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use epub_reader::{
    decode_image,
    parse_markdown,
    parse_txt,
    Block,
    Document,
    Epub,
    GrayImage,
    Span,
    Style,
};
use esp_hal::gpio::{Level, Output, OutputConfig};
use t5s3_epaper_core::{
    sdcard::{Error, PinConfig},
    Display,
    SdCard,
};
use u8g2_fonts::{
    fonts,
    types::{FontColor, VerticalPosition},
    FontRenderer,
};

use crate::layout::{SCREEN_W, STATUS_H};

const MARGIN_X: i32 = 24;
const CONTENT_TOP: i32 = 70;
const CONTENT_BOTTOM: i32 = 905;
const CONTENT_H: i32 = CONTENT_BOTTOM - CONTENT_TOP;
const LINE_H: i32 = 24;
const BLANK_H: i32 = 12;
const CHAR_W: i32 = 9;
const FOOTER_Y: i32 = 935;
const PREV_ZONE_W: i32 = 180;
const CHARS_PER_LINE: usize = ((SCREEN_W - 2 * MARGIN_X) / CHAR_W) as usize;
// 8.3-safe so embedded-sdmmc can create it: a <=8 char dir, and per-book
// progress files named from a 32-bit path hash (8 hex chars) with a 3-char
// extension. FAT cannot create long/LFN names.
const PROGRESS_DIR: &str = "/READER";

// 9x15 monospace u8g2 fonts: a Latin-extended face (ASCII + accents), a
// Cyrillic fallback, and an ASCII bold face. all share the 9px cell, so the
// fixed-width wrapping math is unchanged; glyphs are routed per-character by
// Unicode range.
static FONT_REGULAR: FontRenderer = FontRenderer::new::<fonts::u8g2_font_9x15_te>();
static FONT_CYRILLIC: FontRenderer = FontRenderer::new::<fonts::u8g2_font_9x15_t_cyrillic>();
static FONT_BOLD: FontRenderer = FontRenderer::new::<fonts::u8g2_font_9x15B_tr>();

struct Segment {
    text: String,
    bold: bool,
}

// a laid-out display line. heights drive pagination; `Blank` is the gap between
// paragraphs and around headings. `Image` is rendered alone on its own page, so
// its height never participates in line packing.
enum Line {
    Blank,
    Rule,
    Image(String),
    Text(Vec<Segment>),
    Heading(Vec<Segment>),
}

impl Line {
    fn height(&self) -> i32 {
        match self {
            Line::Image(_) => 0,
            Line::Blank => BLANK_H,
            _ => LINE_H,
        }
    }
}

// where chapters come from. txt/md parse fully up front (they are small and
// single-chapter); epub keeps only its bytes + spine resident and parses each
// chapter on demand, so a whole book is never in memory at once.
enum Source {
    Memory(Document),
    Book(Epub),
}

// an open book positioned at one page. only the current chapter is laid out at
// a time; turning past a chapter boundary parses and paginates the neighbour.
pub(crate) struct ReaderDoc {
    source: Source,
    // content-hash key of the file, used to name its progress file so a
    // bookmark follows the book across moves/renames.
    key: u32,
    chapter_count: usize,
    chapter: usize,
    lines: Vec<Line>,
    pages: Vec<usize>,
    page: usize,
}

impl ReaderDoc {
    fn new(source: Source, key: u32, chapter: usize, page: usize) -> Self {
        let chapter_count = match &source {
            Source::Memory(doc) => doc.chapters().len().max(1),
            Source::Book(epub) => epub.chapter_count().max(1),
        };
        let chapter = chapter.min(chapter_count.saturating_sub(1));
        let lines = layout_chapter(&source, chapter);
        let pages = paginate(&lines);
        let page = page.min(pages.len().saturating_sub(1));
        Self {
            source,
            key,
            chapter_count,
            chapter,
            lines,
            pages,
            page,
        }
    }

    fn load_chapter(&mut self, chapter: usize, at_end: bool) {
        self.chapter = chapter;
        self.lines = layout_chapter(&self.source, chapter);
        self.pages = paginate(&self.lines);
        self.page = if at_end {
            self.pages.len().saturating_sub(1)
        } else {
            0
        };
    }

    pub(crate) fn next_page(&mut self) -> bool {
        if self.page + 1 < self.pages.len() {
            self.page += 1;
            true
        } else if self.chapter + 1 < self.chapter_count {
            self.load_chapter(self.chapter + 1, false);
            true
        } else {
            false
        }
    }

    pub(crate) fn prev_page(&mut self) -> bool {
        if self.page > 0 {
            self.page -= 1;
            true
        } else if self.chapter > 0 {
            self.load_chapter(self.chapter - 1, true);
            true
        } else {
            false
        }
    }

    // persist the current chapter/page to the book's progress file, keyed by
    // content hash so it survives the file being moved or renamed.
    pub(crate) fn save(&self) {
        let Ok((_lora_cs, card)) = mount() else {
            return;
        };
        card.create_dir_all(PROGRESS_DIR).ok();
        let body = format!("{} {}", self.chapter, self.page);
        if let Err(e) = card.write_file(&progress_path(self.key), body.as_bytes()) {
            esp_println::println!("reader: save progress failed: {e:?}");
        }
    }
}

pub(crate) enum Tap {
    Prev,
    Next,
    None,
}

pub(crate) fn is_reader(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, ext)| {
        ext.eq_ignore_ascii_case("txt")
            || ext.eq_ignore_ascii_case("md")
            || ext.eq_ignore_ascii_case("markdown")
            || ext.eq_ignore_ascii_case("epub")
    })
}

// read a supported file from the card and open it at `chapter`/`page`. mounts
// the card the same self-contained way as the file browser. returns a short
// human-readable message on failure so the caller can show why it failed.
pub(crate) fn load_document(path: &str) -> Result<ReaderDoc, String> {
    let (_lora_cs, card) = mount().map_err(|e| format!("SD init failed: {e:?}"))?;
    let bytes = card
        .read_file(path)
        .map_err(|e| format!("read failed: {e:?}"))?;

    // key the bookmark by file contents (not path) so it follows moves/renames.
    let key = fnv1a(&bytes) as u32;
    let (chapter, page) = read_progress(&card, key);

    let ext = path.rsplit_once('.').map_or("", |(_, ext)| ext);
    let source = if ext.eq_ignore_ascii_case("epub") {
        Source::Book(Epub::open(bytes).map_err(|e| {
            esp_println::println!("reader: open {path} failed: {e}");
            format!("parse failed: {e}")
        })?)
    } else {
        let parsed = if ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown") {
            parse_markdown(&bytes)
        } else if ext.eq_ignore_ascii_case("txt") {
            parse_txt(&bytes)
        } else {
            return Err(format!("unsupported file type: .{ext}"));
        };
        Source::Memory(parsed.map_err(|e| {
            esp_println::println!("reader: parse {path} failed: {e}");
            format!("parse failed: {e}")
        })?)
    };

    Ok(ReaderDoc::new(source, key, chapter, page))
}

pub(crate) fn draw(display: &mut Display, doc: &ReaderDoc) {
    let page = doc.page.min(doc.pages.len().saturating_sub(1));
    let start = doc.pages.get(page).copied().unwrap_or(0);
    let end = doc.pages.get(page + 1).copied().unwrap_or(doc.lines.len());

    let mut y = CONTENT_TOP + 18;
    for line in &doc.lines[start..end] {
        match line {
            Line::Blank => {}
            Line::Image(href) => render_image(display, &doc.source, href),
            Line::Rule => {
                Rectangle::new(
                    Point::new(MARGIN_X, y - 8),
                    Size::new((SCREEN_W - 2 * MARGIN_X) as u32, 2),
                )
                .into_styled(PrimitiveStyle::with_fill(Gray4::new(6)))
                .draw(display)
                .ok();
            }
            Line::Text(segments) => draw_segments(display, segments, y, false),
            Line::Heading(segments) => draw_segments(display, segments, y, true),
        }
        y += line.height();
    }

    let footer = if doc.chapter_count > 1 {
        format!(
            "< ch {}/{}    p {}/{} >",
            doc.chapter + 1,
            doc.chapter_count,
            page + 1,
            doc.pages.len()
        )
    } else {
        format!("< prev    {} / {}    next >", page + 1, doc.pages.len())
    };
    Text::with_alignment(
        &footer,
        Point::new(SCREEN_W / 2, FOOTER_Y),
        MonoTextStyle::new(&FONT_9X15, Gray4::new(4)),
        Alignment::Center,
    )
    .draw(display)
    .ok();
}

pub(crate) fn tap_zone(sx: i32, sy: i32) -> Tap {
    if sy < STATUS_H {
        Tap::None
    } else if sx < PREV_ZONE_W {
        Tap::Prev
    } else {
        Tap::Next
    }
}

// read the saved (chapter, page) for `key` using an already-mounted card,
// defaulting to the start if there is no readable bookmark.
fn read_progress(card: &SdCard, key: u32) -> (usize, usize) {
    let Ok(bytes) = card.read_file(&progress_path(key)) else {
        return (0, 0);
    };
    core::str::from_utf8(&bytes)
        .ok()
        .and_then(|text| {
            let mut parts = text.split_whitespace();
            let chapter = parts.next()?.parse().ok()?;
            let page = parts.next()?.parse().ok()?;
            Some((chapter, page))
        })
        .unwrap_or((0, 0))
}

fn draw_segments(display: &mut Display, segments: &[Segment], baseline: i32, force_bold: bool) {
    let mut x = MARGIN_X;
    for segment in segments {
        let bold = force_bold || segment.bold;
        for ch in segment.text.chars() {
            let pos = Point::new(x, baseline);
            let color = FontColor::Transparent(Gray4::BLACK);
            // fall back to '?' for glyphs no face has (e.g. Greek, CJK).
            if pick_font(ch, bold)
                .render(ch, pos, VerticalPosition::Baseline, color, display)
                .is_err()
            {
                FONT_REGULAR
                    .render('?', pos, VerticalPosition::Baseline, color, display)
                    .ok();
            }
            x += CHAR_W;
        }
    }
}

// route a glyph to a face that has it: Cyrillic to the Cyrillic font, ASCII in
// bold context to the bold face, everything else (incl. accented Latin) to the
// regular face. all three share the 9px advance, so x steps by CHAR_W per
// glyph.
fn pick_font(ch: char, bold: bool) -> &'static FontRenderer {
    if ('\u{0400}'..='\u{04FF}').contains(&ch) {
        &FONT_CYRILLIC
    } else if bold && ch.is_ascii() {
        &FONT_BOLD
    } else {
        &FONT_REGULAR
    }
}

// fetch, decode and draw an image filling the content area, or a placeholder if
// it can't be read/decoded (missing, unsupported, or over the size budget).
fn render_image(display: &mut Display, source: &Source, href: &str) {
    let decoded = match source {
        Source::Book(epub) => epub
            .read_resource(href)
            .ok()
            .and_then(|bytes| decode_image(&bytes).ok()),
        Source::Memory(_) => None,
    };
    match decoded {
        Some(image) => draw_image(display, &image),
        None => {
            Text::with_alignment(
                "[image unavailable]",
                Point::new(SCREEN_W / 2, (CONTENT_TOP + CONTENT_BOTTOM) / 2),
                MonoTextStyle::new(&FONT_9X15, Gray4::new(6)),
                Alignment::Center,
            )
            .draw(display)
            .ok();
        }
    }
}

// scale an image to fit the content box (preserving aspect), nearest-neighbour
// sampled and ordered-dithered to the panel's 16 gray levels, centered.
fn draw_image(display: &mut Display, image: &GrayImage) {
    let box_w = (SCREEN_W - 2 * MARGIN_X) as u32;
    let box_h = (CONTENT_BOTTOM - CONTENT_TOP) as u32;
    let src_w = u32::from(image.width()).max(1);
    let src_h = u32::from(image.height()).max(1);

    let (dst_w, dst_h) = if box_w * src_h <= box_h * src_w {
        (box_w, (box_w * src_h / src_w).max(1))
    } else {
        ((box_h * src_w / src_h).max(1), box_h)
    };

    let off_x = MARGIN_X + ((box_w - dst_w) / 2) as i32;
    let off_y = CONTENT_TOP + ((box_h - dst_h) / 2) as i32;
    let area = Rectangle::new(Point::new(off_x, off_y), Size::new(dst_w, dst_h));

    let colors = (0..dst_h).flat_map(move |dy| {
        (0..dst_w).map(move |dx| {
            let sx = (dx * src_w / dst_w) as u16;
            let sy = (dy * src_h / dst_h) as u16;
            Gray4::new(dither(image.sample(sx, sy), dx, dy))
        })
    });
    display.fill_contiguous(&area, colors).ok();
}

// 4x4 ordered (Bayer) dithering of an 8-bit luma value to a 0..=15 gray level.
fn dither(luma: u8, x: u32, y: u32) -> u8 {
    const BAYER: [[u32; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
    let scaled = u32::from(luma) * 15;
    let base = scaled / 255;
    let frac = scaled % 255;
    let threshold = BAYER[(y & 3) as usize][(x & 3) as usize];
    let level = if frac * 16 / 255 > threshold {
        base + 1
    } else {
        base
    };
    level.min(15) as u8
}

// lay out a single chapter's blocks into display lines. parsing the chapter
// from an epub happens here, on demand, so only one chapter is resident.
fn layout_chapter(source: &Source, chapter: usize) -> Vec<Line> {
    match source {
        Source::Memory(doc) => doc
            .chapters()
            .get(chapter)
            .map(|c| layout_blocks(c.blocks()))
            .unwrap_or_default(),
        Source::Book(epub) => match epub.chapter(chapter) {
            Ok(c) => layout_blocks(c.blocks()),
            Err(e) => {
                esp_println::println!("reader: chapter {chapter} failed: {e}");
                Vec::new()
            }
        },
    }
}

fn layout_blocks(blocks: &[Block]) -> Vec<Line> {
    let mut lines = Vec::new();
    for block in blocks {
        match block {
            Block::Heading { spans, .. } => {
                if !lines.is_empty() {
                    lines.push(Line::Blank);
                }
                for segments in wrap(spans) {
                    lines.push(Line::Heading(segments));
                }
                lines.push(Line::Blank);
            }
            Block::Paragraph(spans) => {
                for segments in wrap(spans) {
                    lines.push(Line::Text(segments));
                }
                lines.push(Line::Blank);
            }
            Block::Rule => lines.push(Line::Rule),
            Block::Image { href } => lines.push(Line::Image(href.clone())),
        }
    }
    lines
}

fn paginate(lines: &[Line]) -> Vec<usize> {
    let mut starts = vec![0];
    let mut height = 0;
    for (i, line) in lines.iter().enumerate() {
        // an image takes its own page: close the current page (if any) at it and
        // start a fresh page for whatever follows.
        if matches!(line, Line::Image(_)) {
            if height > 0 {
                starts.push(i);
            }
            if i + 1 < lines.len() {
                starts.push(i + 1);
            }
            height = 0;
            continue;
        }
        let line_h = line.height();
        if height > 0 && height + line_h > CONTENT_H {
            starts.push(i);
            height = 0;
        }
        height += line_h;
    }
    starts
}

// greedy word-wrap a paragraph's styled spans into display lines of at most
// CHARS_PER_LINE columns. words longer than a line are hard-split. italic and
// inline-code fall back to the regular face; only bold has a distinct font.
fn wrap(spans: &[Span]) -> Vec<Vec<Segment>> {
    let mut lines: Vec<Vec<Segment>> = Vec::new();
    let mut current: Vec<Segment> = Vec::new();
    let mut current_len = 0;

    for span in spans {
        let bold = span.style().contains(Style::BOLD);
        let text = asciify(span.text());
        for word in text.split_whitespace() {
            let word_len = word.chars().count();

            if word_len > CHARS_PER_LINE {
                if current_len > 0 {
                    lines.push(core::mem::take(&mut current));
                    current_len = 0;
                }
                let mut chars = word.chars().peekable();
                while chars.peek().is_some() {
                    let chunk: String = chars.by_ref().take(CHARS_PER_LINE).collect();
                    let chunk_len = chunk.chars().count();
                    if chunk_len == CHARS_PER_LINE {
                        lines.push(vec![Segment { text: chunk, bold }]);
                    } else {
                        current.push(Segment { text: chunk, bold });
                        current_len = chunk_len;
                    }
                }
                continue;
            }

            let needed = if current_len == 0 {
                word_len
            } else {
                current_len + 1 + word_len
            };
            if current_len > 0 && needed > CHARS_PER_LINE {
                lines.push(core::mem::take(&mut current));
                current_len = 0;
            }
            push_word(&mut current, &mut current_len, word, bold);
        }
    }
    if current_len > 0 {
        lines.push(current);
    }
    lines
}

fn push_word(current: &mut Vec<Segment>, current_len: &mut usize, word: &str, bold: bool) {
    let word_len = word.chars().count();
    if *current_len == 0 {
        current.push(Segment {
            text: String::from(word),
            bold,
        });
        *current_len = word_len;
        return;
    }
    match current.last_mut() {
        Some(last) if last.bold == bold => {
            last.text.push(' ');
            last.text.push_str(word);
        }
        _ => {
            let mut text = String::from(" ");
            text.push_str(word);
            current.push(Segment { text, bold });
        }
    }
    *current_len += 1 + word_len;
}

// normalize typographic punctuation that the fonts lack (dashes, ellipsis,
// curly quotes, exotic spaces) to ASCII; pass letters through so the Cyrillic /
// Latin-extended faces can render them. done before wrapping so column counts
// stay accurate.
fn asciify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\u{2014}' | '\u{2015}' => out.push_str("--"),
            '\u{2013}' => out.push('-'),
            '\u{2011}' => out.push('-'),
            '\u{2018}' | '\u{2019}' | '\u{201B}' => out.push('\''),
            '\u{201C}' | '\u{201D}' | '\u{201F}' => out.push('"'),
            '\u{2026}' => out.push_str("..."),
            '\u{00A0}' | '\u{2007}' | '\u{202F}' => out.push(' '),
            '\u{00AD}' => {}
            _ => out.push(c),
        }
    }
    out
}

fn progress_path(key: u32) -> String {
    format!("{PROGRESS_DIR}/{key:08X}.POS")
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

// mount the SD card, mirroring the file browser: steal the shared SPI2 pins and
// hold the LoRa chip-select high to release MISO. the returned guard must stay
// alive for the duration of card access.
fn mount() -> Result<(Output<'static>, SdCard<'static>), Error> {
    let lora_cs = Output::new(
        unsafe { esp_hal::peripherals::GPIO46::steal() },
        Level::High,
        OutputConfig::default(),
    );
    let pins = PinConfig {
        miso: unsafe { esp_hal::peripherals::GPIO21::steal() },
        mosi: unsafe { esp_hal::peripherals::GPIO13::steal() },
        sclk: unsafe { esp_hal::peripherals::GPIO14::steal() },
        cs: unsafe { esp_hal::peripherals::GPIO12::steal() },
    };
    let spi = unsafe { esp_hal::peripherals::SPI2::steal() };
    let card = SdCard::new(pins, spi)?;
    Ok((lora_cs, card))
}
