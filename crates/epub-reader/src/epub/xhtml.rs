use alloc::{string::String, vec::Vec};

use xmlparser::{ElementEnd, Token, Tokenizer};

use crate::{
    document::{Block, Span, Style},
    error::Error,
};

// turn one XHTML content document into a flat list of blocks. block-level tags
// (p, h1-h6, li, blockquote, div) open a new block; b/strong, i/em and code
// toggle inline style; br is a soft space, hr a rule, img an image reference.
// unknown tags are transparent, so their text still flows into the document.
pub(super) fn parse(xhtml: &[u8]) -> Result<Vec<Block>, Error> {
    let text = core::str::from_utf8(xhtml).map_err(Error::NotUtf8)?;
    let mut builder = Builder::default();

    for token in Tokenizer::from(text) {
        match token.map_err(|_| Error::Malformed {
            context: String::from("xhtml: invalid xml"),
        })? {
            Token::ElementStart { local, .. } => builder.start(local.as_str()),
            Token::Attribute { local, value, .. } => {
                builder.attribute(local.as_str(), value.as_str());
            }
            Token::ElementEnd { end, .. } => match end {
                ElementEnd::Open => {}
                ElementEnd::Empty => {
                    let name = builder.pending.clone();
                    builder.end(&name);
                }
                ElementEnd::Close(_, local) => builder.end(local.as_str()),
            },
            Token::Text { text } => builder.text(text.as_str()),
            _ => {}
        }
    }

    Ok(builder.finish())
}

#[derive(Default)]
struct Builder {
    blocks: Vec<Block>,
    spans: Vec<Span>,
    run: String,
    run_style: Style,
    bold: u32,
    italic: u32,
    code: u32,
    heading_level: Option<u8>,
    pending: String,
    skip: u32,
}

impl Builder {
    fn start(&mut self, name: &str) {
        self.pending = ascii_lower(name);
        if matches!(self.pending.as_str(), "head" | "script" | "style") {
            self.skip += 1;
            return;
        }
        if let Some(level) = heading_level(&self.pending) {
            self.flush_block();
            self.heading_level = Some(level);
        } else if is_block(&self.pending) {
            self.flush_block();
        }
        match self.pending.as_str() {
            "b" | "strong" => self.toggle(|b| b.bold += 1),
            "i" | "em" => self.toggle(|b| b.italic += 1),
            "code" | "tt" | "kbd" => self.toggle(|b| b.code += 1),
            "br" => self.run.push(' '),
            "hr" => {
                self.flush_block();
                self.blocks.push(Block::Rule);
            }
            _ => {}
        }
    }

    fn attribute(&mut self, name: &str, value: &str) {
        if self.skip > 0 {
            return;
        }
        if self.pending == "img" && (name == "src" || name == "href") {
            self.flush_block();
            self.blocks.push(Block::Image {
                href: String::from(value),
            });
        }
    }

    fn end(&mut self, name: &str) {
        let name = ascii_lower(name);
        if matches!(name.as_str(), "head" | "script" | "style") {
            self.skip = self.skip.saturating_sub(1);
            return;
        }
        match name.as_str() {
            "b" | "strong" => self.toggle(|b| b.bold = b.bold.saturating_sub(1)),
            "i" | "em" => self.toggle(|b| b.italic = b.italic.saturating_sub(1)),
            "code" | "tt" | "kbd" => self.toggle(|b| b.code = b.code.saturating_sub(1)),
            _ => {}
        }
        if heading_level(&name).is_some() || is_block(&name) {
            self.flush_block();
        }
    }

    fn text(&mut self, raw: &str) {
        if self.skip > 0 {
            return;
        }
        let style = self.current_style();
        if !self.run.is_empty() && style != self.run_style {
            self.flush_run();
        }
        self.run_style = style;
        unescape_into(raw, &mut self.run);
    }

    fn finish(mut self) -> Vec<Block> {
        self.flush_block();
        self.blocks
    }

    fn toggle(&mut self, f: impl FnOnce(&mut Self)) {
        self.flush_run();
        f(self);
    }

    fn current_style(&self) -> Style {
        let mut style = Style::empty();
        style.set(Style::BOLD, self.bold > 0 || self.heading_level.is_some());
        style.set(Style::ITALIC, self.italic > 0);
        style.set(Style::CODE, self.code > 0);
        style
    }

    fn flush_run(&mut self) {
        if !self.run.is_empty() {
            self.spans
                .push(Span::new(core::mem::take(&mut self.run), self.run_style));
        }
    }

    fn flush_block(&mut self) {
        self.flush_run();
        // whitespace-only blocks come from insignificant inter-tag indentation;
        // drop them so they don't become blank paragraphs.
        if self.spans.iter().any(|s| !s.text().trim().is_empty()) {
            let spans = core::mem::take(&mut self.spans);
            match self.heading_level.take() {
                Some(level) => self.blocks.push(Block::Heading { level, spans }),
                None => self.blocks.push(Block::Paragraph(spans)),
            }
        } else {
            self.spans.clear();
        }
        self.heading_level = None;
    }
}

fn heading_level(name: &str) -> Option<u8> {
    match name {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    }
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div" | "li" | "blockquote" | "section" | "article" | "figcaption"
    )
}

fn ascii_lower(s: &str) -> String {
    s.chars().map(|c| c.to_ascii_lowercase()).collect()
}

// resolve the five predefined XML entities plus decimal/hex numeric character
// references, appending the decoded text to `out`. unknown entities are kept
// verbatim so no text is silently dropped.
fn unescape_into(raw: &str, out: &mut String) {
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'&' {
            // copy the run up to the next '&' as a whole str to stay utf-8 safe
            let start = i;
            while i < bytes.len() && bytes[i] != b'&' {
                i += 1;
            }
            out.push_str(&raw[start..i]);
            continue;
        }
        if let Some(rel) = raw[i..].find(';') {
            let entity = &raw[i + 1..i + rel];
            if let Some(c) = decode_entity(entity) {
                out.push(c);
            } else {
                out.push_str(&raw[i..=i + rel]);
            }
            i += rel + 1;
        } else {
            out.push('&');
            i += 1;
        }
    }
}

fn decode_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        _ => {
            let code = if let Some(hex) = entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
            {
                u32::from_str_radix(hex, 16).ok()?
            } else if let Some(dec) = entity.strip_prefix('#') {
                dec.parse().ok()?
            } else {
                return None;
            };
            char::from_u32(code)
        }
    }
}
