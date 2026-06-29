use alloc::string::String;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("source is not valid UTF-8")]
    NotUtf8(#[source] core::str::Utf8Error),
    #[error("document contained no readable text")]
    Empty,
    #[error("malformed {context}")]
    Malformed { context: String },
    #[error("invalid zip archive: {0}")]
    Zip(&'static str),
    #[error("failed to inflate zip entry {entry}")]
    Inflate { entry: String },
    #[error("zip entry not found: {0}")]
    MissingEntry(String),
    #[error("image: {0}")]
    Image(&'static str),
}
