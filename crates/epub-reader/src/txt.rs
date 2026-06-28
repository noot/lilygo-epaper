use alloc::{string::String, vec, vec::Vec};

use crate::{
    document::{Block, Chapter, Document, Meta, Span},
    error::Error,
};

/// Parse plain UTF-8 text into a single-chapter document. Blank lines separate
/// paragraphs; consecutive non-blank lines are joined with a space and left for
/// the renderer to soft-wrap.
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
        if line.is_empty() {
            flush(&mut blocks, &mut paragraph);
        } else {
            if !paragraph.is_empty() {
                paragraph.push(' ');
            }
            paragraph.push_str(line.trim());
        }
    }
    flush(&mut blocks, &mut paragraph);

    if blocks.is_empty() {
        return Err(Error::Empty);
    }

    Ok(Document::new(
        Meta::default(),
        vec![Chapter::new(None, blocks)],
    ))
}

fn flush(blocks: &mut Vec<Block>, paragraph: &mut String) {
    if !paragraph.is_empty() {
        blocks.push(Block::Paragraph(vec![Span::plain(core::mem::take(
            paragraph,
        ))]));
    }
}
