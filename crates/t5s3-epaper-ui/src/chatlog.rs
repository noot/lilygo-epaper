//! SD-card persistence for the mesh chat log.
//!
//! Every message shown on the receive tab (and each one sent) is appended as
//! a record to `MESH/CHAT.LOG`: a 16-hex `(origin, msg_id)` dedup key, `|`,
//! then the display line with its stamp. The keys make the log's dedup
//! outlive the engine — after a reboot a recap replays the relays' last 24 h,
//! and without persisted keys those arrivals would duplicate the restored
//! backlog (0 = keyless, e.g. own sent lines, exempt from matching). At load
//! time the tail becomes the receive tab's backlog; past the size bound the
//! file is compacted down to that same tail. A missing or unreadable card
//! degrades to RAM-only chat.

use alloc::{string::String, vec::Vec};

use t5s3_epaper_core::{spi::Bus, SdCard};

use crate::pages::lora::RECV_MAX;

const DIR: &str = "MESH";
const PATH: &str = "MESH/CHAT.LOG";

/// compaction threshold: at ~130 bytes per stamped line this is well past
/// what the ram backlog retains, so compaction is rare.
const MAX_BYTES: u64 = 128 * 1024;

/// Read the retained tail of the chat log as parallel key + display-line
/// vectors, and its size for the append-side growth accounting. Records
/// without a key prefix (pre-key logs) load as keyless.
pub(crate) fn load(bus: &Bus<'static>) -> (Vec<u64>, Vec<String>, u64) {
    let card = match SdCard::new(bus) {
        Ok(card) => card,
        Err(e) => {
            esp_println::println!("chatlog: mount failed: {e:?}");
            return (Vec::new(), Vec::new(), 0);
        }
    };
    if !card.exists(PATH).unwrap_or(false) {
        return (Vec::new(), Vec::new(), 0);
    }
    let bytes = match card.read_file(PATH) {
        Ok(bytes) => bytes,
        Err(e) => {
            esp_println::println!("chatlog: read failed: {e:?}");
            return (Vec::new(), Vec::new(), 0);
        }
    };
    let size = bytes.len() as u64;
    let text = String::from_utf8_lossy(&bytes);
    let mut keys = Vec::new();
    let mut lines = Vec::new();
    for record in text.lines() {
        match record.split_once('|') {
            Some((key, line)) if key.len() == 16 => {
                keys.push(u64::from_str_radix(key, 16).unwrap_or(0));
                lines.push(String::from(line));
            }
            _ => {
                keys.push(0);
                lines.push(String::from(record));
            }
        }
    }
    if lines.len() > RECV_MAX {
        keys.drain(..keys.len() - RECV_MAX);
        lines.drain(..lines.len() - RECV_MAX);
    }
    (keys, lines, size)
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

/// Append one keyed message record, compacting the file back down to the ram
/// backlog (`keys` zipped with `lines`) once it exceeds the growth bound.
/// `size` tracks the file's length across appends so the bound costs no
/// metadata reads.
pub(crate) fn append(
    bus: &Bus<'static>,
    key: u64,
    line: &str,
    keys: &[u64],
    lines: &[String],
    size: &mut u64,
) {
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
        for (entry_key, entry) in keys.iter().zip(lines) {
            compacted.push_str(&alloc::format!("{entry_key:016x}|{entry}\n"));
        }
        match card.write_file(PATH, compacted.as_bytes()) {
            Ok(()) => *size = compacted.len() as u64,
            Err(e) => esp_println::println!("chatlog: compact failed: {e:?}"),
        }
        return;
    }
    let record = alloc::format!("{key:016x}|{line}\n");
    match card.append_file(PATH, record.as_bytes()) {
        Ok(()) => *size += record.len() as u64,
        Err(e) => esp_println::println!("chatlog: append failed: {e:?}"),
    }
}
