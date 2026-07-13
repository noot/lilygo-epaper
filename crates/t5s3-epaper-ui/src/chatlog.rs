//! SD-card persistence for the mesh chat log.
//!
//! Every message shown on the receive tab (and each one sent) is appended as
//! a line to `MESH/CHAT.LOG`, already carrying its display stamp, so the log
//! survives sleep and reboots. At load time the tail of the file becomes the
//! receive tab's backlog; when the file outgrows its bound it is compacted
//! down to that same tail, keeping writes cheap (one append per message)
//! without unbounded growth. A missing or unreadable card degrades to
//! RAM-only chat.

use alloc::{string::String, vec::Vec};

use t5s3_epaper_core::{spi::Bus, SdCard};

use crate::pages::lora::RECV_MAX;

const DIR: &str = "MESH";
const PATH: &str = "MESH/CHAT.LOG";

/// compaction threshold: at ~130 bytes per stamped line this is well past
/// what the ram backlog retains, so compaction is rare.
const MAX_BYTES: u64 = 128 * 1024;

/// Read the retained tail of the chat log, and its current size for the
/// append-side growth accounting.
pub(crate) fn load(bus: &Bus<'static>) -> (Vec<String>, u64) {
    let card = match SdCard::new(bus) {
        Ok(card) => card,
        Err(e) => {
            esp_println::println!("chatlog: mount failed: {e:?}");
            return (Vec::new(), 0);
        }
    };
    if !card.exists(PATH).unwrap_or(false) {
        return (Vec::new(), 0);
    }
    let bytes = match card.read_file(PATH) {
        Ok(bytes) => bytes,
        Err(e) => {
            esp_println::println!("chatlog: read failed: {e:?}");
            return (Vec::new(), 0);
        }
    };
    let size = bytes.len() as u64;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    if lines.len() > RECV_MAX {
        lines.drain(..lines.len() - RECV_MAX);
    }
    (lines, size)
}

/// Delete the persisted history (the ram log is the caller's to clear).
pub(crate) fn clear(bus: &Bus<'static>, size: &mut u64) {
    let card = match SdCard::new(bus) {
        Ok(card) => card,
        Err(e) => {
            esp_println::println!("chatlog: mount failed: {e:?}");
            return;
        }
    };
    if !card.exists(PATH).unwrap_or(false) {
        *size = 0;
        return;
    }
    match card.write_file(PATH, b"") {
        Ok(()) => *size = 0,
        Err(e) => esp_println::println!("chatlog: clear failed: {e:?}"),
    }
}

/// Append one message line, compacting the file back down to `entries` (the
/// ram backlog) once it exceeds the growth bound. `size` tracks the file's
/// length across appends so the bound costs no metadata reads.
pub(crate) fn append(bus: &Bus<'static>, line: &str, entries: &[String], size: &mut u64) {
    let card = match SdCard::new(bus) {
        Ok(card) => card,
        Err(e) => {
            esp_println::println!("chatlog: mount failed: {e:?}");
            return;
        }
    };
    card.create_dir_all(DIR).ok();
    if *size > MAX_BYTES {
        let mut compacted = String::new();
        for entry in entries {
            compacted.push_str(entry);
            compacted.push('\n');
        }
        match card.write_file(PATH, compacted.as_bytes()) {
            Ok(()) => *size = compacted.len() as u64,
            Err(e) => esp_println::println!("chatlog: compact failed: {e:?}"),
        }
        return;
    }
    let mut record = String::with_capacity(line.len() + 1);
    record.push_str(line);
    record.push('\n');
    match card.append_file(PATH, record.as_bytes()) {
        Ok(()) => *size += record.len() as u64,
        Err(e) => esp_println::println!("chatlog: append failed: {e:?}"),
    }
}
