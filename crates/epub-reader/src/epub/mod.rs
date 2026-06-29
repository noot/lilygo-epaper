mod container;
mod opf;
mod xhtml;
mod zip;

use alloc::{format, string::String, vec::Vec};

use crate::{
    document::{Block, Chapter, Document, Meta},
    error::Error,
};

/// A lazily-read EPUB: the compressed archive bytes plus its resolved spine and
/// metadata. Chapters are parsed one at a time via [`Epub::chapter`] so a whole
/// book is never resident in memory at once.
pub struct Epub {
    bytes: Vec<u8>,
    spine: Vec<String>,
    meta: Meta,
}

impl Epub {
    /// Read an EPUB's container and OPF package, taking ownership of its bytes.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if the archive, its container, or its OPF package
    /// cannot be read.
    pub fn open(bytes: Vec<u8>) -> Result<Self, Error> {
        let (spine, meta) = {
            let archive = zip::Archive::open(&bytes)?;
            let container = archive.read("META-INF/container.xml")?;
            let opf_path = container::opf_path(&container)?;
            let package = opf::parse(&archive.read(&opf_path)?)?;
            let base = dir_of(&opf_path);
            let spine = package
                .spine_hrefs
                .iter()
                .map(|href| resolve(base, href))
                .collect();
            (spine, Meta::new(package.title, package.author))
        };
        Ok(Self { bytes, spine, meta })
    }

    #[must_use]
    pub fn chapter_count(&self) -> usize {
        self.spine.len()
    }

    #[must_use]
    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    /// Inflate and parse a single spine item into a [`Chapter`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::MissingEntry`] if `index` is out of range, or a parse
    /// error if the spine item cannot be read or parsed.
    pub fn chapter(&self, index: usize) -> Result<Chapter, Error> {
        let href = self
            .spine
            .get(index)
            .ok_or_else(|| Error::MissingEntry(format!("spine item {index}")))?;
        let archive = zip::Archive::open(&self.bytes)?;
        let mut blocks = xhtml::parse(&archive.read(href)?)?;
        // image src attributes are relative to the chapter file; rewrite them to
        // full archive paths so the caller can fetch them via `read_resource`.
        let base = dir_of(href);
        for block in &mut blocks {
            if let Block::Image { href } = block {
                *href = resolve(base, href);
            }
        }
        Ok(Chapter::new(None, blocks))
    }

    /// Read a raw archive entry (e.g. an image) by its full path.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if the archive is invalid or the entry is missing.
    pub fn read_resource(&self, path: &str) -> Result<Vec<u8>, Error> {
        zip::Archive::open(&self.bytes)?.read(path)
    }
}

/// Parse the bytes of an `.epub` file into a [`Document`], one chapter per
/// spine item in reading order.
///
/// # Errors
///
/// Returns an [`Error`] if the archive, its container, or its OPF package
/// cannot be read, or [`Error::Empty`] if no spine item yielded any text.
/// Individual spine items that are missing or fail to parse are skipped.
pub fn parse(bytes: &[u8]) -> Result<Document, Error> {
    let epub = Epub::open(bytes.to_vec())?;
    let mut chapters = Vec::new();
    for index in 0..epub.chapter_count() {
        if let Ok(chapter) = epub.chapter(index) {
            if !chapter.is_empty() {
                chapters.push(chapter);
            }
        }
    }

    if chapters.is_empty() {
        return Err(Error::Empty);
    }
    Ok(Document::new(epub.meta().clone(), chapters))
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

// resolve a manifest href (relative to the OPF directory) to a normalized,
// percent-decoded archive path, dropping any fragment.
fn resolve(base: &str, href: &str) -> String {
    let href = href.split('#').next().unwrap_or(href);
    let combined = if let Some(rest) = href.strip_prefix('/') {
        String::from(rest)
    } else if base.is_empty() {
        String::from(href)
    } else {
        format!("{base}/{href}")
    };
    normalize(&percent_decode(&combined))
}

fn normalize(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            segment => out.push(segment),
        }
    }
    out.join("/")
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
