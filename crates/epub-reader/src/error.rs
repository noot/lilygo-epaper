use alloc::string::String;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("source is not valid UTF-8")]
    NotUtf8(#[source] core::str::Utf8Error),
    #[error("document contained no readable text")]
    Empty,
    #[error("malformed {context}")]
    Malformed { context: String },
}
