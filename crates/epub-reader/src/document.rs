use alloc::{string::String, vec::Vec};

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Style: u8 {
        const BOLD = 0b0000_0001;
        const ITALIC = 0b0000_0010;
        const CODE = 0b0000_0100;
    }
}

/// A run of text sharing a single style. The smallest renderable unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    text: String,
    style: Style,
}

impl Span {
    #[must_use]
    pub fn new(text: String, style: Style) -> Self {
        Self { text, style }
    }

    #[must_use]
    pub fn plain(text: String) -> Self {
        Self {
            text,
            style: Style::empty(),
        }
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn style(&self) -> Style {
        self.style
    }
}

/// A block-level element in reading order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Heading { level: u8, spans: Vec<Span> },
    Paragraph(Vec<Span>),
    Image { href: String },
    Rule,
}

/// One spine item / source section, holding its blocks in reading order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Chapter {
    title: Option<String>,
    blocks: Vec<Block>,
}

impl Chapter {
    #[must_use]
    pub fn new(title: Option<String>, blocks: Vec<Block>) -> Self {
        Self { title, blocks }
    }

    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    #[must_use]
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    title: Option<String>,
    author: Option<String>,
}

impl Meta {
    #[must_use]
    pub fn new(title: Option<String>, author: Option<String>) -> Self {
        Self { title, author }
    }

    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    #[must_use]
    pub fn author(&self) -> Option<&str> {
        self.author.as_deref()
    }
}

/// The format-agnostic model every source parser produces and the UI renders.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Document {
    meta: Meta,
    chapters: Vec<Chapter>,
}

impl Document {
    #[must_use]
    pub fn new(meta: Meta, chapters: Vec<Chapter>) -> Self {
        Self { meta, chapters }
    }

    #[must_use]
    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    #[must_use]
    pub fn chapters(&self) -> &[Chapter] {
        &self.chapters
    }
}
