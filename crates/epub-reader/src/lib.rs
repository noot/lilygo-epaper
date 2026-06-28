#![no_std]

extern crate alloc;

mod document;
mod error;
mod markdown;
mod txt;

pub use document::{Block, Chapter, Document, Meta, Span, Style};
pub use error::Error;
pub use markdown::parse as parse_markdown;
pub use txt::parse as parse_txt;
