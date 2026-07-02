use alloc::{format, string::String, vec, vec::Vec};
use core::cell::RefCell;

use embedded_graphics::{
    mono_font::{ascii::FONT_9X15, MonoTextStyle},
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use epub_reader::{
    chapter_number,
    decode_image,
    parse_markdown,
    parse_txt,
    Block,
    Document,
    Epub,
    Span,
    Style,
};
use esp_hal::{
    gpio::{Level, Output, OutputConfig},
    spi::master::Spi,
    Blocking,
};
use t5s3_epaper_core::{sdcard::Error, Display, SdCard};
use u8g2_fonts::{
    fonts,
    types::{FontColor, VerticalPosition},
    FontRenderer,
};

use crate::{
    layout::{SCREEN_W, STATUS_H},
    settings::{FontFamily, FontSize, LineSpacing, ReaderStyle},
};

const MARGIN_X: i32 = 24;
const CONTENT_TOP: i32 = 70;
const CONTENT_BOTTOM: i32 = 905;
const CONTENT_H: i32 = CONTENT_BOTTOM - CONTENT_TOP;
const FOOTER_Y: i32 = 935;
const PREV_ZONE_W: i32 = 180;
// 8.3-safe so embedded-sdmmc can create it: a <=8 char dir, and per-book
// progress files named from a 32-bit path hash (8 hex chars) with a 3-char
// extension. FAT cannot create long/LFN names.
const PROGRESS_DIR: &str = "/READER";

// reading faces at three sizes: Sans is Helvetica, Serif is New Century
// Schoolbook (both proportional, regular + bold, Latin-extended), and Mono is
// the X11 9x15 / 10x20 bitmap faces plus Inconsolata 24. Cyrillic (which none
// of these cover, except via the monospace fallback) routes to a size-matched
// monospace Cyrillic face. the layout measures every glyph's advance, so the
// mixed widths still lay out correctly. faces with no bold variant (10x20,
// inr24) reuse their regular face for bold context.
static SANS_S_REG: FontRenderer = FontRenderer::new::<fonts::u8g2_font_helvR14_te>();
static SANS_S_BOLD: FontRenderer = FontRenderer::new::<fonts::u8g2_font_helvB14_te>();
static SANS_M_REG: FontRenderer = FontRenderer::new::<fonts::u8g2_font_helvR18_te>();
static SANS_M_BOLD: FontRenderer = FontRenderer::new::<fonts::u8g2_font_helvB18_te>();
static SANS_L_REG: FontRenderer = FontRenderer::new::<fonts::u8g2_font_helvR24_te>();
static SANS_L_BOLD: FontRenderer = FontRenderer::new::<fonts::u8g2_font_helvB24_te>();
static SERIF_S_REG: FontRenderer = FontRenderer::new::<fonts::u8g2_font_ncenR14_te>();
static SERIF_S_BOLD: FontRenderer = FontRenderer::new::<fonts::u8g2_font_ncenB14_te>();
static SERIF_M_REG: FontRenderer = FontRenderer::new::<fonts::u8g2_font_ncenR18_te>();
static SERIF_M_BOLD: FontRenderer = FontRenderer::new::<fonts::u8g2_font_ncenB18_te>();
static SERIF_L_REG: FontRenderer = FontRenderer::new::<fonts::u8g2_font_ncenR24_te>();
static SERIF_L_BOLD: FontRenderer = FontRenderer::new::<fonts::u8g2_font_ncenB24_te>();
static MONO_S_REG: FontRenderer = FontRenderer::new::<fonts::u8g2_font_9x15_te>();
static MONO_S_BOLD: FontRenderer = FontRenderer::new::<fonts::u8g2_font_9x15B_tr>();
static MONO_M_REG: FontRenderer = FontRenderer::new::<fonts::u8g2_font_10x20_te>();
static MONO_L_REG: FontRenderer = FontRenderer::new::<fonts::u8g2_font_inr24_mf>();
static CYR_S: FontRenderer = FontRenderer::new::<fonts::u8g2_font_9x15_t_cyrillic>();
static CYR_M: FontRenderer = FontRenderer::new::<fonts::u8g2_font_10x20_t_cyrillic>();
static CYR_L: FontRenderer = FontRenderer::new::<fonts::u8g2_font_inr24_t_cyrillic>();

// usable text width between the margins.
const CONTENT_W: i32 = SCREEN_W - 2 * MARGIN_X;
// the printable ASCII range whose advances are precomputed per document.
const ASCII_LO: u8 = 0x20;
const ASCII_HI: u8 = 0x7E;
const ASCII_N: usize = (ASCII_HI - ASCII_LO) as usize + 1;

// metrics for one reader style: the resolved family/size, line and paragraph
// heights, plus a table of ASCII glyph advances (regular and bold) so width-
// based wrapping doesn't have to look up the font for every common character.
#[derive(Clone, Copy)]
struct Metrics {
    family: FontFamily,
    size: FontSize,
    line_h: i32,
    blank_h: i32,
    ascii_regular: [i16; ASCII_N],
    ascii_bold: [i16; ASCII_N],
}

impl Metrics {
    fn new(style: ReaderStyle) -> Self {
        let line_h = line_height(style.size, style.spacing);
        let mut ascii_regular = [0i16; ASCII_N];
        let mut ascii_bold = [0i16; ASCII_N];
        for i in 0..ASCII_N {
            let ch = (ASCII_LO + i as u8) as char;
            ascii_regular[i] = measure_char(ch, false, style.family, style.size);
            ascii_bold[i] = measure_char(ch, true, style.family, style.size);
        }
        Self {
            family: style.family,
            size: style.size,
            line_h,
            blank_h: line_h / 2,
            ascii_regular,
            ascii_bold,
        }
    }
}

// line height for a size at a spacing: a per-size base (the "Normal" leading
// that reads well) tightened or loosened by the spacing setting.
fn line_height(size: FontSize, spacing: LineSpacing) -> i32 {
    let base = match size {
        FontSize::Small => 24,
        FontSize::Medium => 28,
        FontSize::Large => 34,
    };
    match spacing {
        LineSpacing::Compact => base - 3,
        LineSpacing::Normal => base,
        LineSpacing::Relaxed => base + 7,
    }
}

// pixel advance of a single character in the face selected for (bold, family,
// size).
fn measure_char(ch: char, bold: bool, family: FontFamily, size: FontSize) -> i16 {
    let mut buf = [0u8; 4];
    let s = ch.encode_utf8(&mut buf);
    pick_font(ch, bold, family, size)
        .get_rendered_dimensions(&*s, Point::zero(), VerticalPosition::Baseline)
        .map(|d| d.advance.x)
        .unwrap_or(0)
        .clamp(0, i32::from(i16::MAX)) as i16
}

// pixel advance of a character, served from the ASCII table where possible and
// measured live for everything else (accents, Cyrillic, ...).
fn char_adv(ch: char, bold: bool, m: &Metrics) -> i32 {
    let cp = ch as u32;
    if (u32::from(ASCII_LO)..=u32::from(ASCII_HI)).contains(&cp) {
        let idx = (cp - u32::from(ASCII_LO)) as usize;
        let table = if bold {
            &m.ascii_bold
        } else {
            &m.ascii_regular
        };
        i32::from(table[idx])
    } else {
        i32::from(measure_char(ch, bold, m.family, m.size))
    }
}

fn word_width(word: &str, bold: bool, m: &Metrics) -> i32 {
    word.chars().map(|c| char_adv(c, bold, m)).sum()
}

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
    fn height(&self, m: &Metrics) -> i32 {
        match self {
            Line::Image(_) => 0,
            Line::Blank => m.blank_h,
            _ => m.line_h,
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
    metrics: Metrics,
    chapter_count: usize,
    // spine indices that begin a real chapter per the book's TOC, used to number
    // chapters against the actual contents rather than raw spine items. empty for
    // txt/markdown and epubs without a usable TOC (footer falls back to spine).
    nav_starts: Vec<usize>,
    chapter: usize,
    lines: Vec<Line>,
    pages: Vec<usize>,
    page: usize,
}

impl ReaderDoc {
    fn new(source: Source, key: u32, chapter: usize, page: usize, style: ReaderStyle) -> Self {
        let metrics = Metrics::new(style);
        let chapter_count = match &source {
            Source::Memory(doc) => doc.chapters().len().max(1),
            Source::Book(epub) => epub.chapter_count().max(1),
        };
        let nav_starts = match &source {
            Source::Book(epub) => epub.nav_starts().to_vec(),
            Source::Memory(_) => Vec::new(),
        };
        let chapter = chapter.min(chapter_count.saturating_sub(1));
        let lines = layout_chapter(&source, chapter, &metrics);
        let pages = paginate(&lines, &metrics);
        let page = page.min(pages.len().saturating_sub(1));
        Self {
            source,
            key,
            metrics,
            chapter_count,
            nav_starts,
            chapter,
            lines,
            pages,
            page,
        }
    }

    fn load_chapter(&mut self, chapter: usize, at_end: bool) {
        self.chapter = chapter;
        self.lines = layout_chapter(&self.source, chapter, &self.metrics);
        self.pages = paginate(&self.lines, &self.metrics);
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
    // content hash so it survives the file being moved or renamed. the current
    // section's page count is stored too, so the library can draw a page-refined
    // overall-progress bar without having to paginate the book itself.
    pub(crate) fn save(&self) {
        let Ok(bus) = sd_bus() else {
            return;
        };
        let Ok((_lora_cs, card)) = mount(&bus) else {
            return;
        };
        card.create_dir_all(PROGRESS_DIR).ok();
        let body = format!("{} {} {}", self.chapter, self.page, self.pages.len());
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
pub(crate) fn load_document(path: &str, style: ReaderStyle) -> Result<ReaderDoc, String> {
    let bus = sd_bus().map_err(|e| format!("SD init failed: {e:?}"))?;
    let (_lora_cs, card) = mount(&bus).map_err(|e| format!("SD init failed: {e:?}"))?;
    let bytes = card
        .read_file(path)
        .map_err(|e| format!("read failed: {e:?}"))?;

    // key the bookmark by file contents (not path) so it follows moves/renames.
    let key = content_key(&bytes);
    let Progress { chapter, page, .. } = read_progress(&card, key);

    let ext = path.rsplit_once('.').map_or("", |(_, ext)| ext);
    let source = if ext.eq_ignore_ascii_case("epub") {
        let epub = Epub::open(bytes).map_err(|e| {
            esp_println::println!("reader: open {path} failed: {e}");
            format!("parse failed: {e}")
        })?;
        esp_println::println!(
            "reader: {path} spine={} toc_chapters={}",
            epub.chapter_count(),
            epub.nav_starts().len()
        );
        Source::Book(epub)
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

    Ok(ReaderDoc::new(source, key, chapter, page, style))
}

pub(crate) fn draw(display: &mut Display, doc: &ReaderDoc) {
    let page = doc.page.min(doc.pages.len().saturating_sub(1));
    let start = doc.pages.get(page).copied().unwrap_or(0);
    let end = doc.pages.get(page + 1).copied().unwrap_or(doc.lines.len());

    let m = &doc.metrics;
    let mut y = CONTENT_TOP + m.line_h - 6;
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
            Line::Text(segments) => draw_segments(display, segments, y, false, m),
            Line::Heading(segments) => draw_segments(display, segments, y, true, m),
        }
        y += line.height(m);
    }

    let footer = match chapter_number(&doc.nav_starts, doc.chapter) {
        // real chapters known from the TOC; chapter 0 is front matter.
        Some((0, _)) => format!("< front matter    p {}/{} >", page + 1, doc.pages.len()),
        Some((cur, total)) => {
            format!("< ch {cur}/{total}    p {}/{} >", page + 1, doc.pages.len())
        }
        // no usable TOC and many spine items (e.g. a size-split book): the spine
        // isn't chapters, so show honest overall progress instead of a fake count.
        None if doc.chapter_count > 1 => format!(
            "< {}%    p {}/{} >",
            doc.chapter * 100 / doc.chapter_count,
            page + 1,
            doc.pages.len()
        ),
        None => format!("< prev    {} / {}    next >", page + 1, doc.pages.len()),
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

// content-hash key of a file's bytes, used to name its progress file so a
// bookmark follows the book across moves/renames. shared with the library page
// so both derive the same key.
pub(crate) fn content_key(bytes: &[u8]) -> u32 {
    fnv1a(bytes) as u32
}

// the saved reading position: the chapter (spine index), the page within it,
// and that section's page count at save time (0 for older bookmarks that
// predate the page-count field, or when unknown).
pub(crate) struct Progress {
    pub(crate) chapter: usize,
    pub(crate) page: usize,
    pub(crate) page_count: usize,
}

// read the saved position for `key` using an already-mounted card, defaulting
// to the start if there is no readable bookmark.
pub(crate) fn read_progress(card: &SdCard, key: u32) -> Progress {
    let default = Progress {
        chapter: 0,
        page: 0,
        page_count: 0,
    };
    let Ok(bytes) = card.read_file(&progress_path(key)) else {
        return default;
    };
    core::str::from_utf8(&bytes)
        .ok()
        .and_then(|text| {
            let mut parts = text.split_whitespace();
            let chapter = parts.next()?.parse().ok()?;
            let page = parts.next()?.parse().ok()?;
            let page_count = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            Some(Progress {
                chapter,
                page,
                page_count,
            })
        })
        .unwrap_or(default)
}

fn draw_segments(
    display: &mut Display,
    segments: &[Segment],
    baseline: i32,
    force_bold: bool,
    m: &Metrics,
) {
    let mut x = MARGIN_X;
    for segment in segments {
        let bold = force_bold || segment.bold;
        for ch in segment.text.chars() {
            let pos = Point::new(x, baseline);
            let color = FontColor::Transparent(Gray4::BLACK);
            // fall back to '?' for glyphs no face has (e.g. Greek, CJK).
            if pick_font(ch, bold, m.family, m.size)
                .render(ch, pos, VerticalPosition::Baseline, color, display)
                .is_err()
            {
                pick_font('?', false, m.family, m.size)
                    .render('?', pos, VerticalPosition::Baseline, color, display)
                    .ok();
            }
            x += char_adv(ch, bold, m);
        }
    }
}

// route a glyph to a face that has it: Cyrillic to the (monospace) Cyrillic
// fallback, everything else (ASCII and accented Latin) to the selected family's
// regular or bold face. proportional widths are handled by `char_adv`, so faces
// need not share an advance.
fn pick_font(ch: char, bold: bool, family: FontFamily, size: FontSize) -> &'static FontRenderer {
    if ('\u{0400}'..='\u{04FF}').contains(&ch) {
        return match size {
            FontSize::Small => &CYR_S,
            FontSize::Medium => &CYR_M,
            FontSize::Large => &CYR_L,
        };
    }
    let (regular, bolded) = match (family, size) {
        (FontFamily::Sans, FontSize::Small) => (&SANS_S_REG, &SANS_S_BOLD),
        (FontFamily::Sans, FontSize::Medium) => (&SANS_M_REG, &SANS_M_BOLD),
        (FontFamily::Sans, FontSize::Large) => (&SANS_L_REG, &SANS_L_BOLD),
        (FontFamily::Serif, FontSize::Small) => (&SERIF_S_REG, &SERIF_S_BOLD),
        (FontFamily::Serif, FontSize::Medium) => (&SERIF_M_REG, &SERIF_M_BOLD),
        (FontFamily::Serif, FontSize::Large) => (&SERIF_L_REG, &SERIF_L_BOLD),
        // 10x20 and inr24 ship no bold face, so bold reuses the regular one.
        (FontFamily::Mono, FontSize::Small) => (&MONO_S_REG, &MONO_S_BOLD),
        (FontFamily::Mono, FontSize::Medium) => (&MONO_M_REG, &MONO_M_REG),
        (FontFamily::Mono, FontSize::Large) => (&MONO_L_REG, &MONO_L_REG),
    };
    if bold {
        bolded
    } else {
        regular
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
        Some(image) => {
            let box_w = (SCREEN_W - 2 * MARGIN_X) as u32;
            let box_h = (CONTENT_BOTTOM - CONTENT_TOP) as u32;
            crate::widgets::draw_image_fit(display, &image, MARGIN_X, CONTENT_TOP, box_w, box_h);
        }
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

// lay out a single chapter's blocks into display lines. parsing the chapter
// from an epub happens here, on demand, so only one chapter is resident.
fn layout_chapter(source: &Source, chapter: usize, m: &Metrics) -> Vec<Line> {
    match source {
        Source::Memory(doc) => doc
            .chapters()
            .get(chapter)
            .map(|c| layout_blocks(c.blocks(), m))
            .unwrap_or_default(),
        Source::Book(epub) => match epub.chapter(chapter) {
            Ok(c) => layout_blocks(c.blocks(), m),
            Err(e) => {
                esp_println::println!("reader: chapter {chapter} failed: {e}");
                Vec::new()
            }
        },
    }
}

fn layout_blocks(blocks: &[Block], m: &Metrics) -> Vec<Line> {
    let mut lines = Vec::new();
    for block in blocks {
        match block {
            Block::Heading { spans, .. } => {
                if !lines.is_empty() {
                    lines.push(Line::Blank);
                }
                for segments in wrap(spans, true, m) {
                    lines.push(Line::Heading(segments));
                }
                lines.push(Line::Blank);
            }
            Block::Paragraph(spans) => {
                for segments in wrap(spans, false, m) {
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

fn paginate(lines: &[Line], m: &Metrics) -> Vec<usize> {
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
        let line_h = line.height(m);
        if height > 0 && height + line_h > CONTENT_H {
            starts.push(i);
            height = 0;
        }
        height += line_h;
    }
    starts
}

// greedy word-wrap a paragraph's styled spans into display lines no wider than
// the content area, measuring each word's pixel width. `force_bold` makes the
// whole run bold (used for headings). words wider than a line are hard-split.
// italic and inline-code fall back to the regular face; bold has its own face.
fn wrap(spans: &[Span], force_bold: bool, m: &Metrics) -> Vec<Vec<Segment>> {
    let mut lines: Vec<Vec<Segment>> = Vec::new();
    let mut current: Vec<Segment> = Vec::new();
    let mut current_w = 0;

    for span in spans {
        let bold = force_bold || span.style().contains(Style::BOLD);
        let text = asciify(span.text());
        let space_w = char_adv(' ', bold, m);
        for word in text.split_whitespace() {
            let ww = word_width(word, bold, m);

            if ww > CONTENT_W {
                if current_w > 0 {
                    lines.push(core::mem::take(&mut current));
                    current_w = 0;
                }
                split_word(word, bold, m, &mut lines, &mut current, &mut current_w);
                continue;
            }

            let needed = if current_w == 0 {
                ww
            } else {
                current_w + space_w + ww
            };
            if current_w > 0 && needed > CONTENT_W {
                lines.push(core::mem::take(&mut current));
                current_w = 0;
            }
            push_word(&mut current, &mut current_w, word, bold, space_w, ww);
        }
    }
    if current_w > 0 {
        lines.push(current);
    }
    lines
}

fn push_word(
    current: &mut Vec<Segment>,
    current_w: &mut i32,
    word: &str,
    bold: bool,
    space_w: i32,
    ww: i32,
) {
    if *current_w == 0 {
        current.push(Segment {
            text: String::from(word),
            bold,
        });
        *current_w = ww;
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
    *current_w += space_w + ww;
}

// hard-split a word wider than the content area into chunks that fit, flushing
// full chunks as their own lines and leaving the remainder in `current`.
fn split_word(
    word: &str,
    bold: bool,
    m: &Metrics,
    lines: &mut Vec<Vec<Segment>>,
    current: &mut Vec<Segment>,
    current_w: &mut i32,
) {
    let mut chunk = String::new();
    let mut w = 0;
    for ch in word.chars() {
        let cw = char_adv(ch, bold, m);
        if w > 0 && w + cw > CONTENT_W {
            lines.push(vec![Segment {
                text: core::mem::take(&mut chunk),
                bold,
            }]);
            w = 0;
        }
        chunk.push(ch);
        w += cw;
    }
    if !chunk.is_empty() {
        current.push(Segment { text: chunk, bold });
        *current_w = w;
    }
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
