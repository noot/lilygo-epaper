use alloc::{string::String, vec::Vec};

use xmlparser::{ElementEnd, Token, Tokenizer};

// extract the ordered content targets (hrefs, possibly with fragments) from a
// table-of-contents document. `is_ncx` selects the EPUB2 NCX parser over the
// EPUB3 navigation-document parser. hrefs are relative to the TOC document and
// resolved by the caller; the returned order is document order.
pub(super) fn targets(xml: &[u8], is_ncx: bool) -> Vec<String> {
    let Ok(text) = core::str::from_utf8(xml) else {
        return Vec::new();
    };
    if is_ncx {
        ncx_targets(text)
    } else {
        nav_targets(text)
    }
}

// EPUB3 nav document: collect `<a href>` links. links inside a `<nav>` whose
// `epub:type` is "toc" win; otherwise fall back to links inside any nav, then
// to every link in the document (for minimal nav docs without a typed nav
// wrapper).
fn nav_targets(text: &str) -> Vec<String> {
    let mut toc_links: Vec<String> = Vec::new();
    let mut nav_links: Vec<String> = Vec::new();
    let mut all_links: Vec<String> = Vec::new();

    // navs are effectively un-nested in practice; a stack keeps us honest if
    // they ever are. each entry marks whether that nav is the toc.
    let mut nav_stack: Vec<bool> = Vec::new();
    let mut element: Option<String> = None;
    let mut pending_nav = false;
    let mut pending_is_toc = false;
    let mut current_href: Option<String> = None;

    for token in Tokenizer::from(text).flatten() {
        match token {
            Token::ElementStart { local, .. } => {
                element = Some(String::from(local.as_str()));
                if local.as_str() == "nav" {
                    pending_nav = true;
                    pending_is_toc = false;
                } else if local.as_str() == "a" {
                    current_href = None;
                }
            }
            Token::Attribute { local, value, .. } => match element.as_deref() {
                Some("nav") if local.as_str() == "type" => {
                    pending_is_toc = value.as_str().split_ascii_whitespace().any(|t| t == "toc");
                }
                Some("a") if local.as_str() == "href" => {
                    current_href = Some(String::from(value.as_str()));
                }
                _ => {}
            },
            Token::ElementEnd { end, .. } => match end {
                ElementEnd::Open => {
                    if pending_nav {
                        nav_stack.push(pending_is_toc);
                        pending_nav = false;
                    }
                }
                ElementEnd::Empty => pending_nav = false,
                ElementEnd::Close(_, name) => {
                    if name.as_str() == "nav" {
                        nav_stack.pop();
                    } else if name.as_str() == "a" {
                        if let Some(href) = current_href.take() {
                            all_links.push(href.clone());
                            if !nav_stack.is_empty() {
                                nav_links.push(href.clone());
                            }
                            if nav_stack.last() == Some(&true) {
                                toc_links.push(href);
                            }
                        }
                    }
                }
            },
            _ => {}
        }
    }

    if !toc_links.is_empty() {
        toc_links
    } else if !nav_links.is_empty() {
        nav_links
    } else {
        all_links
    }
}

// EPUB2 NCX: collect `<content src>` from within `<navMap>`, ignoring the
// pageList / navList sections that share the element.
fn ncx_targets(text: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut in_navmap = false;
    let mut element: Option<String> = None;

    for token in Tokenizer::from(text).flatten() {
        match token {
            Token::ElementStart { local, .. } => {
                element = Some(String::from(local.as_str()));
            }
            Token::Attribute { local, value, .. } => {
                if in_navmap && element.as_deref() == Some("content") && local.as_str() == "src" {
                    targets.push(String::from(value.as_str()));
                }
            }
            Token::ElementEnd { end, .. } => match end {
                ElementEnd::Open => {
                    if element.as_deref() == Some("navMap") {
                        in_navmap = true;
                    }
                }
                ElementEnd::Close(_, name) => {
                    if name.as_str() == "navMap" {
                        in_navmap = false;
                    }
                }
                ElementEnd::Empty => {}
            },
            _ => {}
        }
    }

    targets
}
