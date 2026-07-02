use alloc::{string::String, vec::Vec};

use xmlparser::{ElementEnd, Token, Tokenizer};

use crate::error::Error;

// the parts of an OPF package the reader needs: optional metadata, the spine's
// reading order resolved to manifest hrefs (relative to the OPF), and the cover
// image's href when one could be identified.
pub(super) struct Package {
    pub(super) title: Option<String>,
    pub(super) author: Option<String>,
    pub(super) spine_hrefs: Vec<String>,
    pub(super) cover_href: Option<String>,
    // the table-of-contents document's href (relative to the OPF): the EPUB3
    // navigation document if present, else the EPUB2 NCX. used to number real
    // chapters instead of raw spine items.
    pub(super) toc_href: Option<String>,
    // whether `toc_href` refers to an NCX (EPUB2) rather than a nav document.
    pub(super) toc_is_ncx: bool,
}

// one manifest entry, tracking the attributes needed to both resolve the spine
// and identify the cover image.
struct Item {
    id: String,
    href: String,
    media_type: Option<String>,
    properties: Option<String>,
}

// accumulates the OPF's title/author/manifest/spine as the tokenizer streams,
// tracking the element and per-item/meta attributes currently open.
#[derive(Default)]
struct Builder {
    title: Option<String>,
    author: Option<String>,
    manifest: Vec<Item>,
    spine_ids: Vec<String>,
    // the manifest id referenced by `<meta name="cover" content="...">` (EPUB2).
    meta_cover_id: Option<String>,
    // the manifest id referenced by `<spine toc="...">` (EPUB2 NCX).
    spine_toc_id: Option<String>,

    element: Option<String>,
    item_id: Option<String>,
    item_href: Option<String>,
    item_media: Option<String>,
    item_props: Option<String>,
    meta_name: Option<String>,
    meta_content: Option<String>,
}

impl Builder {
    fn open(&mut self, name: &str) {
        self.element = Some(String::from(name));
        if name == "item" {
            self.item_id = None;
            self.item_href = None;
            self.item_media = None;
            self.item_props = None;
        } else if name == "meta" {
            self.meta_name = None;
            self.meta_content = None;
        }
    }

    fn attribute(&mut self, attr: &str, value: &str) {
        match self.element.as_deref() {
            Some("item") => match attr {
                "id" => self.item_id = Some(String::from(value)),
                "href" => self.item_href = Some(String::from(value)),
                "media-type" => self.item_media = Some(String::from(value)),
                "properties" => self.item_props = Some(String::from(value)),
                _ => {}
            },
            Some("itemref") if attr == "idref" => self.spine_ids.push(String::from(value)),
            Some("spine") if attr == "toc" => self.spine_toc_id = Some(String::from(value)),
            Some("meta") => match attr {
                "name" => self.meta_name = Some(String::from(value)),
                "content" => self.meta_content = Some(String::from(value)),
                _ => {}
            },
            _ => {}
        }
    }

    fn text(&mut self, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        match self.element.as_deref() {
            Some("title") if self.title.is_none() => self.title = Some(String::from(trimmed)),
            Some("creator") if self.author.is_none() => self.author = Some(String::from(trimmed)),
            _ => {}
        }
    }

    // on an element close, commit any fully-read item or cover meta.
    fn close(&mut self) {
        if let (Some(id), Some(href)) = (self.item_id.take(), self.item_href.take()) {
            self.manifest.push(Item {
                id,
                href,
                media_type: self.item_media.take(),
                properties: self.item_props.take(),
            });
        }
        if let (Some(name), Some(content)) = (self.meta_name.take(), self.meta_content.take()) {
            if name.eq_ignore_ascii_case("cover") && self.meta_cover_id.is_none() {
                self.meta_cover_id = Some(content);
            }
        }
    }
}

pub(super) fn parse(xml: &[u8]) -> Result<Package, Error> {
    let text = core::str::from_utf8(xml).map_err(Error::NotUtf8)?;
    let mut b = Builder::default();

    for token in Tokenizer::from(text) {
        match token.map_err(|_| Error::Malformed {
            context: String::from("content.opf: invalid xml"),
        })? {
            Token::ElementStart { local, .. } => b.open(local.as_str()),
            Token::Attribute { local, value, .. } => b.attribute(local.as_str(), value.as_str()),
            Token::Text { text } => b.text(text.as_str()),
            Token::ElementEnd { end, .. } => {
                if matches!(end, ElementEnd::Empty | ElementEnd::Close(..)) {
                    b.close();
                    if !matches!(end, ElementEnd::Open) {
                        b.element = None;
                    }
                }
            }
            _ => {}
        }
    }

    let spine_hrefs = b
        .spine_ids
        .iter()
        .filter_map(|id| {
            b.manifest
                .iter()
                .find(|item| &item.id == id)
                .map(|item| item.href.clone())
        })
        .collect();

    let cover_href = find_cover(&b.manifest, b.meta_cover_id.as_deref());
    let (toc_href, toc_is_ncx) = find_toc(&b.manifest, b.spine_toc_id.as_deref());

    Ok(Package {
        title: b.title,
        author: b.author,
        spine_hrefs,
        cover_href,
        toc_href,
        toc_is_ncx,
    })
}

// locate the table-of-contents document: prefer the EPUB3 navigation document
// (`properties="nav"`), else the EPUB2 NCX (referenced by `<spine toc>` or a
// manifest item with the NCX media type). the bool marks the NCX case, which
// needs a different parser.
fn find_toc(manifest: &[Item], spine_toc_id: Option<&str>) -> (Option<String>, bool) {
    if let Some(item) = manifest.iter().find(|item| {
        item.properties
            .as_deref()
            .is_some_and(|p| p.split_ascii_whitespace().any(|t| t == "nav"))
    }) {
        return (Some(item.href.clone()), false);
    }

    if let Some(id) = spine_toc_id {
        if let Some(item) = manifest.iter().find(|item| item.id == id) {
            return (Some(item.href.clone()), true);
        }
    }

    let ncx = manifest
        .iter()
        .find(|item| item.media_type.as_deref() == Some("application/x-dtbncx+xml"))
        .map(|item| item.href.clone());
    (ncx, true)
}

// identify the cover image's href, trying the most authoritative signal first:
// the EPUB3 `properties="cover-image"` manifest flag, then the EPUB2
// `<meta name="cover">` id, then a heuristic on an image item whose id or href
// mentions "cover".
fn find_cover(manifest: &[Item], meta_cover_id: Option<&str>) -> Option<String> {
    if let Some(item) = manifest.iter().find(|item| {
        item.properties
            .as_deref()
            .is_some_and(|p| p.split_ascii_whitespace().any(|t| t == "cover-image"))
    }) {
        return Some(item.href.clone());
    }

    if let Some(id) = meta_cover_id {
        if let Some(item) = manifest.iter().find(|item| item.id == id) {
            return Some(item.href.clone());
        }
    }

    manifest
        .iter()
        .find(|item| is_image(item) && (mentions_cover(&item.id) || mentions_cover(&item.href)))
        .map(|item| item.href.clone())
}

fn is_image(item: &Item) -> bool {
    item.media_type
        .as_deref()
        .is_some_and(|t| t.starts_with("image/"))
}

fn mentions_cover(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.contains("cover")
}
