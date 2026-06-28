use alloc::{string::String, vec, vec::Vec};

use crate::{
    document::{Block, Chapter, Document, Meta, Span, Style},
    error::Error,
};

/// Parse a pragmatic Markdown subset into a single-chapter document: ATX
/// headings (`#`..`######`), thematic breaks (`---`/`***`/`___`), blank-line
/// separated paragraphs, and inline `**bold**`/`*italic*`/`` `code` ``. Block
/// constructs beyond this (lists, tables, links, blockquotes) are rendered as
/// their literal text.
///
/// # Errors
///
/// Returns [`Error::NotUtf8`] if `bytes` is not valid UTF-8, or [`Error::Empty`]
/// if the input holds no renderable text.
pub fn parse(bytes: &[u8]) -> Result<Document, Error> {
    let text = core::str::from_utf8(bytes).map_err(Error::NotUtf8)?;

    let mut blocks = Vec::new();
    let mut paragraph = String::new();

    for line in text.lines() {
        let line = line.trim_end();
        let trimmed = line.trim_start();

        if trimmed.is_empty() {
            flush_paragraph(&mut blocks, &mut paragraph);
            continue;
        }

        if is_thematic_break(trimmed) {
            flush_paragraph(&mut blocks, &mut paragraph);
            blocks.push(Block::Rule);
            continue;
        }

        if let Some((level, rest)) = heading(trimmed) {
            flush_paragraph(&mut blocks, &mut paragraph);
            let spans = parse_inline(rest);
            if !spans.is_empty() {
                blocks.push(Block::Heading { level, spans });
            }
            continue;
        }

        if !paragraph.is_empty() {
            paragraph.push(' ');
        }
        paragraph.push_str(trimmed);
    }
    flush_paragraph(&mut blocks, &mut paragraph);

    if blocks.is_empty() {
        return Err(Error::Empty);
    }

    Ok(Document::new(
        Meta::default(),
        vec![Chapter::new(None, blocks)],
    ))
}

fn flush_paragraph(blocks: &mut Vec<Block>, paragraph: &mut String) {
    if paragraph.is_empty() {
        return;
    }
    let spans = parse_inline(&core::mem::take(paragraph));
    if !spans.is_empty() {
        blocks.push(Block::Paragraph(spans));
    }
}

fn heading(line: &str) -> Option<(u8, &str)> {
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    if (1..=6).contains(&hashes) && line.as_bytes().get(hashes) == Some(&b' ') {
        Some((u8::try_from(hashes).ok()?, line[hashes + 1..].trim()))
    } else {
        None
    }
}

fn is_thematic_break(line: &str) -> bool {
    let bytes = line.as_bytes();
    matches!(bytes.first(), Some(b'-' | b'*' | b'_'))
        && bytes.len() >= 3
        && bytes.iter().all(|&b| b == bytes[0])
}

fn parse_inline(text: &str) -> Vec<Span> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans = Vec::new();
    let mut buf = String::new();
    let mut style = Style::empty();
    let mut i = 0;

    while i < chars.len() {
        match chars[i] {
            '`' => {
                if let Some(end) = (i + 1..chars.len()).find(|&j| chars[j] == '`') {
                    flush_span(&mut spans, &mut buf, style);
                    let code: String = chars[i + 1..end].iter().collect();
                    spans.push(Span::new(code, style | Style::CODE));
                    i = end + 1;
                    continue;
                }
                buf.push('`');
                i += 1;
            }
            marker @ ('*' | '_') => {
                flush_span(&mut spans, &mut buf, style);
                if chars.get(i + 1) == Some(&marker) {
                    style ^= Style::BOLD;
                    i += 2;
                } else {
                    style ^= Style::ITALIC;
                    i += 1;
                }
            }
            c => {
                buf.push(c);
                i += 1;
            }
        }
    }
    flush_span(&mut spans, &mut buf, style);
    spans
}

fn flush_span(spans: &mut Vec<Span>, buf: &mut String, style: Style) {
    if !buf.is_empty() {
        spans.push(Span::new(core::mem::take(buf), style));
    }
}
