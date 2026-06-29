use alloc::string::String;

use xmlparser::{Token, Tokenizer};

use crate::error::Error;

// extract the OPF package path from META-INF/container.xml by returning the
// `full-path` attribute of the first <rootfile> element.
pub(super) fn opf_path(xml: &[u8]) -> Result<String, Error> {
    let text = core::str::from_utf8(xml).map_err(Error::NotUtf8)?;
    for token in Tokenizer::from(text) {
        if let Ok(Token::Attribute { local, value, .. }) = token {
            if local.as_str() == "full-path" {
                return Ok(String::from(value.as_str()));
            }
        }
    }
    Err(Error::Malformed {
        context: String::from("container.xml: no rootfile full-path"),
    })
}
