mod container;
mod opf;
mod toc;
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
    // full archive path of the cover image, if one was identified in the OPF.
    cover_href: Option<String>,
    // spine indices that begin a real chapter per the table of contents, sorted
    // and deduplicated. empty when the book has no usable TOC, in which case the
    // caller falls back to numbering raw spine items.
    nav_starts: Vec<usize>,
}

impl Epub {
    /// Read an EPUB's container and OPF package, taking ownership of its bytes.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if the archive, its container, or its OPF package
    /// cannot be read.
    pub fn open(bytes: Vec<u8>) -> Result<Self, Error> {
        let (spine, meta, cover_href, nav_starts) = {
            let archive = zip::Archive::open(&bytes)?;
            let container = archive.read("META-INF/container.xml")?;
            let opf_path = container::opf_path(&container)?;
            let package = opf::parse(&archive.read(&opf_path)?)?;
            let base = dir_of(&opf_path);
            let spine: Vec<String> = package
                .spine_hrefs
                .iter()
                .map(|href| resolve(base, href))
                .collect();
            let nav_starts = resolve_nav_starts(&archive, base, &package, &spine);
            let cover_href = package.cover_href.map(|href| resolve(base, &href));
            (
                spine,
                Meta::new(package.title, package.author),
                cover_href,
                nav_starts,
            )
        };
        Ok(Self {
            bytes,
            spine,
            meta,
            cover_href,
            nav_starts,
        })
    }

    /// Spine indices that begin a real chapter per the table of contents,
    /// sorted and deduplicated. Empty when the book has no usable TOC.
    #[must_use]
    pub fn nav_starts(&self) -> &[usize] {
        &self.nav_starts
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

    /// The raw (undecoded) bytes of the cover image, if the OPF identified one.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MissingEntry`] if no cover was identified, or a read
    /// error if the identified entry cannot be inflated.
    pub fn cover(&self) -> Result<Vec<u8>, Error> {
        let href = self
            .cover_href
            .as_deref()
            .ok_or_else(|| Error::MissingEntry(String::from("cover image")))?;
        self.read_resource(href)
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

/// Map a reader's current spine index to a real chapter number using the
/// chapter-start indices from [`Epub::nav_starts`].
///
/// Returns `(chapter, total)` where `chapter` is the 1-based real-chapter
/// number and `total` the real-chapter count; a `chapter` of `0` means the
/// position is in front matter, before the first real chapter. Returns `None`
/// when `nav_starts` is empty (no usable TOC), signalling the caller to number
/// raw spine items instead.
#[must_use]
pub fn chapter_number(nav_starts: &[usize], spine_index: usize) -> Option<(usize, usize)> {
    if nav_starts.is_empty() {
        return None;
    }
    let chapter = nav_starts.partition_point(|&start| start <= spine_index);
    Some((chapter, nav_starts.len()))
}

// read and parse the TOC document, mapping each of its content targets to a
// spine index to yield the sorted, deduplicated set of chapter-start indices.
// an absent/unreadable TOC or one that resolves to fewer than two spine items
// yields an empty set, so the caller falls back to raw spine numbering.
fn resolve_nav_starts(
    archive: &zip::Archive,
    base: &str,
    package: &opf::Package,
    spine: &[String],
) -> Vec<usize> {
    let Some(toc_href) = package.toc_href.as_deref() else {
        return Vec::new();
    };
    let toc_path = resolve(base, toc_href);
    let Ok(bytes) = archive.read(&toc_path) else {
        return Vec::new();
    };
    let toc_base = dir_of(&toc_path);

    let mut starts: Vec<usize> = toc::targets(&bytes, package.toc_is_ncx)
        .iter()
        .filter_map(|target| {
            let abs = resolve(toc_base, target);
            spine.iter().position(|href| href == &abs)
        })
        .collect();
    starts.sort_unstable();
    starts.dedup();

    if starts.len() >= 2 {
        starts
    } else {
        Vec::new()
    }
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
            // decode the two hex digits from the raw bytes; slicing the &str
            // here would panic if a '%' were followed by a multi-byte char.
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
