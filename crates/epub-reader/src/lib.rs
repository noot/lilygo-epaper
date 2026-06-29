#![no_std]

extern crate alloc;

mod document;
mod epub;
mod error;
mod image;
mod markdown;
mod txt;

pub use document::{Block, Chapter, Document, Meta, Span, Style};
pub use epub::{parse as parse_epub, Epub};
pub use error::Error;
pub use image::{decode_image, GrayImage};
pub use markdown::parse as parse_markdown;
pub use txt::parse as parse_txt;
