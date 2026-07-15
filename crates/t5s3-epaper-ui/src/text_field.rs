use alloc::string::String;

use embedded_graphics::{
    mono_font::{ascii::FONT_9X15, MonoTextStyle},
    prelude::*,
    primitives::{PrimitiveStyle, PrimitiveStyleBuilder, Rectangle},
    text::Text,
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use t5s3_epaper_core::Display;

use crate::{
    keyboard::{self, Key},
    layout::screen_to_native_rect,
};

// how long a field must go untouched before a pending quick flush's ghosting
// gets cleaned up with a full-quality one; long enough that it doesn't fire
// between characters of the same typing burst (bursts land well under
// this), short enough that it settles right after the user stops.
const SETTLE_IDLE_US: u64 = 400_000;

/// how a field lays out and scrolls its text.
pub(crate) enum Wrap {
    /// wraps at a fixed character count per line, filling the box; a
    /// trailing "_" marks the edit position. used by the lora composer,
    /// which sends a capped message rather than editing a long document.
    CharFill,
    /// word-wraps like a document and, once the wrapped text overflows the
    /// box, shows only the last `visible_lines` rows with a drawn cursor bar
    /// at the edit position. used by the notes editor.
    WordScroll { visible_lines: usize },
    /// single line, truncated to whatever fits, no wrap, no cursor. used by
    /// the wifi password box.
    Truncate,
}

/// what Enter does on a field.
pub(crate) enum EnterKey {
    /// the caller handles it (send / save+join); the field itself does
    /// nothing.
    Submit,
    /// insert a newline into the buffer, same as any other character.
    Newline,
}

/// the result of feeding a keyboard hit into a field.
pub(crate) enum FieldEvent {
    /// the buffer changed; the field already redrew itself into the
    /// framebuffer and marked the pending flush.
    Changed,
    /// Enter was pressed on a `EnterKey::Submit` field.
    Submit,
    /// the key had no effect (backspace on empty, char at the length cap, or
    /// a shift/symbols toggle the field already handled itself).
    None,
}

/// a touch-keyboard-backed text box: owns the box geometry, the keyboard's
/// shift/symbols layer, and the batched "fast draw, then settle to full
/// draw" flush strategy so typing stays responsive under a backlog of
/// queued keystrokes without leaving ghosting behind once it settles.
pub(crate) struct TextField {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    wrap: Wrap,
    enter: EnterKey,
    placeholder: &'static str,
    enter_label: &'static str,
    symbols: bool,
    shift: bool,
    // set when a keystroke changed the buffer but its flush was deferred to
    // the end of this pass's touch-event drain, so a burst of queued taps
    // (typing outpaced the previous flush) batches into one panel write
    // instead of one per character.
    dirty: bool,
    // set when the last flush used the cheaper, fewer-frame waveform (see
    // `Display::flush_partial_quick`) rather than the full one: it leaves a
    // ghosting debt that a later full-quality flush needs to pay off once
    // typing catches up.
    quick_pending: bool,
    chars_this_pass: u32,
    last_edit_us: u64,
}

impl TextField {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        wrap: Wrap,
        enter: EnterKey,
        placeholder: &'static str,
        enter_label: &'static str,
    ) -> Self {
        Self {
            x,
            y,
            w,
            h,
            wrap,
            enter,
            placeholder,
            enter_label,
            symbols: false,
            shift: false,
            dirty: false,
            quick_pending: false,
            chars_this_pass: 0,
            last_edit_us: 0,
        }
    }

    // drop the keyboard back to its lowercase-letters layer; call on
    // re-entering the screen that owns this field.
    pub(crate) fn reset_keyboard(&mut self) {
        self.symbols = false;
        self.shift = false;
    }

    pub(crate) fn native_rect(&self) -> t5s3_epaper_core::display::Rectangle {
        screen_to_native_rect(self.x, self.y, self.w, self.h)
    }

    pub(crate) fn hit_key(&self, sx: i32, sy: i32) -> Option<Key> {
        keyboard::hit(sx, sy, self.symbols, self.shift)
    }

    // box + text only, for a partial redraw of just this field.
    pub(crate) fn draw(&self, display: &mut Display, text: &str) {
        Rectangle::new(
            Point::new(self.x, self.y),
            Size::new(self.w as u32, self.h as u32),
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

        match &self.wrap {
            Wrap::CharFill => self.draw_char_fill(display, text),
            Wrap::WordScroll { visible_lines } => {
                self.draw_word_scroll(display, text, *visible_lines)
            }
            Wrap::Truncate => self.draw_truncate(display, text),
        }
    }

    // box + text + keyboard, for a full-page redraw. no flush: the caller's
    // whole-page draw flushes everything together.
    pub(crate) fn draw_full(&self, display: &mut Display, text: &str) {
        self.draw(display, text);
        keyboard::draw(display, self.symbols, self.shift, self.enter_label);
    }

    // lora composer style: fixed char-count wrap filling the box, trailing
    // cursor makes the edit position visible (a just-typed space is
    // otherwise indistinguishable from nothing).
    fn draw_char_fill(&self, display: &mut Display, text: &str) {
        let font = MonoTextStyle::new(&FONT_9X15, Gray4::BLACK);
        let x = self.x + 12;
        let mut y = self.y + 28;
        if text.is_empty() && !self.placeholder.is_empty() {
            // hint sits after the cursor, in a lighter shade than typed text
            Text::new(
                &alloc::format!(" {}", self.placeholder),
                Point::new(x + 9, y),
                MonoTextStyle::new(&FONT_9X15, Gray4::new(9)),
            )
            .draw(display)
            .ok();
        }

        let shown = alloc::format!("{text}_");
        let per_line = ((self.w - 24) / 9) as usize;
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

    // notes editor style: word-wrap with a scrolled-to-the-end view and a
    // drawn cursor bar at the edit position.
    fn draw_word_scroll(&self, display: &mut Display, text: &str, visible_lines: usize) {
        let font = MonoTextStyle::new(&FONT_9X15, Gray4::BLACK);
        let cols = ((self.w - 24) / 9) as usize;
        let lines = wrap_ranges(text, cols);
        let first = lines.len().saturating_sub(visible_lines);
        let mut y = self.y + 26;
        let mut cursor = Point::new(self.x + 12, y);
        for &(a, b) in &lines[first..] {
            Text::new(&text[a..b], Point::new(self.x + 12, y), font)
                .draw(display)
                .ok();
            cursor = Point::new(self.x + 12 + text[a..b].chars().count() as i32 * 9, y);
            y += 20;
        }
        Rectangle::new(Point::new(cursor.x + 1, cursor.y - 12), Size::new(2, 15))
            .into_styled(PrimitiveStyle::with_fill(Gray4::BLACK))
            .draw(display)
            .ok();
        if text.is_empty() && !self.placeholder.is_empty() {
            Text::new(
                self.placeholder,
                Point::new(cursor.x + 8, cursor.y),
                MonoTextStyle::new(&FONT_9X15, Gray4::new(4)),
            )
            .draw(display)
            .ok();
        }
    }

    // wifi password style: single line, truncated to fit, no wrap.
    fn draw_truncate(&self, display: &mut Display, text: &str) {
        let font = MonoTextStyle::new(&FONT_9X15, Gray4::BLACK);
        let cols = ((self.w as usize - 24) / 9).max(1);
        let shown = match text.char_indices().nth(cols) {
            Some((end, _)) => &text[..end],
            None => text,
        };
        Text::new(
            shown,
            Point::new(self.x + 12, self.y + self.h / 2 + 6),
            font,
        )
        .draw(display)
        .ok();
    }

    // feed a keyboard hit into the field: mutates `buf` for Char/Space/
    // Backspace/newline-Enter (respecting `max_len`, a per-call argument
    // since some callers cap it dynamically), toggles and flushes the
    // keyboard layer for Shift/Symbols, and leaves Enter on a `Submit` field
    // for the caller to act on.
    pub(crate) fn handle_key(
        &mut self,
        display: &mut Display,
        buf: &mut String,
        key: Key,
        max_len: usize,
        now_us: u64,
    ) -> FieldEvent {
        match key {
            Key::Shift => {
                self.shift = !self.shift;
                self.flush_keyboard(display);
                FieldEvent::None
            }
            Key::Symbols => {
                self.symbols = !self.symbols;
                self.flush_keyboard(display);
                FieldEvent::None
            }
            Key::Enter => match self.enter {
                EnterKey::Submit => FieldEvent::Submit,
                EnterKey::Newline => self.push(display, buf, '\n', max_len, now_us),
            },
            Key::Char(c) => self.push(display, buf, c, max_len, now_us),
            Key::Space => self.push(display, buf, ' ', max_len, now_us),
            Key::Backspace => {
                if buf.pop().is_some() {
                    self.draw(display, buf);
                    self.mark_dirty(now_us);
                    FieldEvent::Changed
                } else {
                    FieldEvent::None
                }
            }
        }
    }

    fn push(
        &mut self,
        display: &mut Display,
        buf: &mut String,
        c: char,
        max_len: usize,
        now_us: u64,
    ) -> FieldEvent {
        if buf.len() >= max_len {
            return FieldEvent::None;
        }
        buf.push(c);
        self.draw(display, buf);
        self.mark_dirty(now_us);
        FieldEvent::Changed
    }

    fn mark_dirty(&mut self, now_us: u64) {
        self.dirty = true;
        self.chars_this_pass += 1;
        self.last_edit_us = now_us;
    }

    fn flush_keyboard(&self, display: &mut Display) {
        keyboard::draw(display, self.symbols, self.shift, self.enter_label);
        display.flush_partial_fast(keyboard::native_rect()).ok();
    }

    // call once per event-drain pass while this field's screen is active:
    // flushes whatever the pass changed (the cheaper waveform if more than
    // one keystroke landed in this pass, since the panel is still behind and
    // draining further keystrokes matters more than this one's quality), or
    // pays off a pending cheap flush's ghosting once the field has been idle
    // long enough. resets the pass counter either way.
    pub(crate) fn end_pass(&mut self, display: &mut Display, buf: &str, now_us: u64) {
        if self.dirty {
            if self.chars_this_pass > 1 {
                display.flush_partial_quick(self.native_rect()).ok();
                self.quick_pending = true;
            } else {
                display.flush_partial_fast(self.native_rect()).ok();
                self.quick_pending = false;
            }
            self.dirty = false;
        } else if self.quick_pending && now_us.saturating_sub(self.last_edit_us) > SETTLE_IDLE_US {
            self.draw(display, buf);
            display.flush_partial_fast(self.native_rect()).ok();
            self.quick_pending = false;
        }
        self.chars_this_pass = 0;
    }

    // flush a deferred edit right away instead of waiting for the next
    // end_pass: the screen is being left mid-burst (Home press, back
    // button, tab switch) so there won't be another pass to pay it off.
    pub(crate) fn flush_pending(&mut self, display: &mut Display) {
        if self.dirty {
            display.flush_partial_fast(self.native_rect()).ok();
            self.dirty = false;
            self.quick_pending = false;
        }
    }

    // drop a deferred edit's flags without flushing: for when a full-page
    // redraw is already about to happen anyway (e.g. a successful lora
    // send clears the message), making the partial flush moot.
    pub(crate) fn clear_pending(&mut self) {
        self.dirty = false;
        self.quick_pending = false;
    }
}

// wrap `text` into byte ranges of at most `cols` characters per line,
// breaking at newlines and, when a line overflows, at its last space. the
// final (possibly empty) line is always emitted so the cursor has a home.
fn wrap_ranges(text: &str, cols: usize) -> alloc::vec::Vec<(usize, usize)> {
    let mut lines = alloc::vec::Vec::new();
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
        if count == cols {
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
