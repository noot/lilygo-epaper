use alloc::{string::String, vec::Vec};

use crate::error::Error;

const EOCD_SIG: u32 = 0x0605_4b50;
const CD_SIG: u32 = 0x0201_4b50;
const LOCAL_SIG: u32 = 0x0403_4b50;
const EOCD_MIN: usize = 22;
const METHOD_STORED: u16 = 0;
const METHOD_DEFLATE: u16 = 8;

struct Entry {
    name: String,
    method: u16,
    compressed_size: u32,
    local_header_offset: u32,
}

// a read-only view over a ZIP archive held entirely in memory, indexed by its
// central directory. epub files are small enough to keep resident.
pub(super) struct Archive<'a> {
    data: &'a [u8],
    entries: Vec<Entry>,
}

impl<'a> Archive<'a> {
    pub(super) fn open(data: &'a [u8]) -> Result<Self, Error> {
        let eocd = find_eocd(data).ok_or(Error::Zip("missing end-of-central-directory record"))?;
        let entry_count = u16le(data, eocd + 10).ok_or(Error::Zip("truncated eocd"))? as usize;
        let cd_offset = u32le(data, eocd + 16).ok_or(Error::Zip("truncated eocd"))? as usize;

        let mut entries = Vec::with_capacity(entry_count);
        let mut cursor = cd_offset;
        for _ in 0..entry_count {
            if u32le(data, cursor) != Some(CD_SIG) {
                return Err(Error::Zip("bad central directory signature"));
            }
            let method = u16le(data, cursor + 10).ok_or(Error::Zip("truncated cd entry"))?;
            let compressed_size =
                u32le(data, cursor + 20).ok_or(Error::Zip("truncated cd entry"))?;
            let name_len =
                u16le(data, cursor + 28).ok_or(Error::Zip("truncated cd entry"))? as usize;
            let extra_len =
                u16le(data, cursor + 30).ok_or(Error::Zip("truncated cd entry"))? as usize;
            let comment_len =
                u16le(data, cursor + 32).ok_or(Error::Zip("truncated cd entry"))? as usize;
            let local_header_offset =
                u32le(data, cursor + 42).ok_or(Error::Zip("truncated cd entry"))?;
            let name_start = cursor + 46;
            let name_bytes = data
                .get(name_start..name_start + name_len)
                .ok_or(Error::Zip("truncated cd entry name"))?;
            let name =
                core::str::from_utf8(name_bytes).map_err(|_| Error::Zip("non-utf8 entry name"))?;

            entries.push(Entry {
                name: String::from(name),
                method,
                compressed_size,
                local_header_offset,
            });
            cursor = name_start + name_len + extra_len + comment_len;
        }

        Ok(Self { data, entries })
    }

    pub(super) fn read(&self, name: &str) -> Result<Vec<u8>, Error> {
        let entry = self
            .entries
            .iter()
            .find(|e| e.name == name)
            .ok_or_else(|| Error::MissingEntry(String::from(name)))?;

        let header = entry.local_header_offset as usize;
        if u32le(self.data, header) != Some(LOCAL_SIG) {
            return Err(Error::Zip("bad local header signature"));
        }
        let name_len =
            u16le(self.data, header + 26).ok_or(Error::Zip("truncated local header"))? as usize;
        let extra_len =
            u16le(self.data, header + 28).ok_or(Error::Zip("truncated local header"))? as usize;
        let start = header + 30 + name_len + extra_len;
        let compressed = self
            .data
            .get(start..start + entry.compressed_size as usize)
            .ok_or(Error::Zip("truncated entry data"))?;

        match entry.method {
            METHOD_STORED => Ok(compressed.to_vec()),
            METHOD_DEFLATE => {
                miniz_oxide::inflate::decompress_to_vec(compressed).map_err(|_| Error::Inflate {
                    entry: String::from(name),
                })
            }
            _ => Err(Error::Zip("unsupported compression method")),
        }
    }
}

fn find_eocd(data: &[u8]) -> Option<usize> {
    if data.len() < EOCD_MIN {
        return None;
    }
    // the record may be followed by a comment of up to 0xffff bytes, so scan
    // backwards from the latest position it could start at.
    let scan_start = data.len() - EOCD_MIN;
    let scan_end = scan_start.saturating_sub(0xffff);
    (scan_end..=scan_start)
        .rev()
        .find(|&i| u32le(data, i) == Some(EOCD_SIG))
}

fn u16le(data: &[u8], offset: usize) -> Option<u16> {
    let bytes = data.get(offset..offset + 2)?;
    Some(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn u32le(data: &[u8], offset: usize) -> Option<u32> {
    let bytes = data.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}
