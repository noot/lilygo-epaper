use alloc::{string::String, vec::Vec};

use xmlparser::{ElementEnd, Token, Tokenizer};

use crate::error::Error;

// the parts of an OPF package the reader needs: optional metadata and the
// spine's reading order resolved to manifest hrefs (relative to the OPF).
pub(super) struct Package {
    pub(super) title: Option<String>,
    pub(super) author: Option<String>,
    pub(super) spine_hrefs: Vec<String>,
}

pub(super) fn parse(xml: &[u8]) -> Result<Package, Error> {
    let text = core::str::from_utf8(xml).map_err(Error::NotUtf8)?;

    let mut title = None;
    let mut author = None;
    let mut manifest: Vec<(String, String)> = Vec::new();
    let mut spine_ids: Vec<String> = Vec::new();

    let mut element: Option<String> = None;
    let mut item_id: Option<String> = None;
    let mut item_href: Option<String> = None;

    for token in Tokenizer::from(text) {
        match token.map_err(|_| Error::Malformed {
            context: String::from("content.opf: invalid xml"),
        })? {
            Token::ElementStart { local, .. } => {
                let name = local.as_str();
                element = Some(String::from(name));
                if name == "item" {
                    item_id = None;
                    item_href = None;
                }
            }
            Token::Attribute { local, value, .. } => {
                let attr = local.as_str();
                match element.as_deref() {
                    Some("item") => match attr {
                        "id" => item_id = Some(String::from(value.as_str())),
                        "href" => item_href = Some(String::from(value.as_str())),
                        _ => {}
                    },
                    Some("itemref") if attr == "idref" => {
                        spine_ids.push(String::from(value.as_str()));
                    }
                    _ => {}
                }
            }
            Token::Text { text } => match element.as_deref() {
                Some("title") if title.is_none() => {
                    let trimmed = text.as_str().trim();
                    if !trimmed.is_empty() {
                        title = Some(String::from(trimmed));
                    }
                }
                Some("creator") if author.is_none() => {
                    let trimmed = text.as_str().trim();
                    if !trimmed.is_empty() {
                        author = Some(String::from(trimmed));
                    }
                }
                _ => {}
            },
            Token::ElementEnd { end, .. } => {
                if matches!(end, ElementEnd::Empty | ElementEnd::Close(..)) {
                    if let (Some(id), Some(href)) = (item_id.take(), item_href.take()) {
                        manifest.push((id, href));
                    }
                    if !matches!(end, ElementEnd::Open) {
                        element = None;
                    }
                }
            }
            _ => {}
        }
    }

    let spine_hrefs = spine_ids
        .iter()
        .filter_map(|id| {
            manifest
                .iter()
                .find(|(item_id, _)| item_id == id)
                .map(|(_, href)| String::from(href))
        })
        .collect();

    Ok(Package {
        title,
        author,
        spine_hrefs,
    })
}
