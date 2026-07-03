use alloc::vec;

use embedded_storage::{ReadStorage as _, Storage as _};
use esp_bootloader_esp_idf::partitions::{
    read_partition_table,
    DataPartitionSubType,
    PartitionType,
    PARTITION_TABLE_MAX_LEN,
};
use esp_storage::FlashStorage;

// reader text size. all three are monospace u8g2 faces with full latin-extended
// and cyrillic coverage, so the reader's fixed-width wrapping math holds; only
// the cell metrics change. see `pages::reader`.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum FontSize {
    Small,
    Medium,
    Large,
}

impl FontSize {
    pub(crate) fn next(self) -> Self {
        match self {
            FontSize::Small => FontSize::Medium,
            FontSize::Medium => FontSize::Large,
            FontSize::Large => FontSize::Small,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            FontSize::Small => "Small",
            FontSize::Medium => "Medium",
            FontSize::Large => "Large",
        }
    }

    fn to_byte(self) -> u8 {
        match self {
            FontSize::Small => 0,
            FontSize::Medium => 1,
            FontSize::Large => 2,
        }
    }

    fn from_byte(b: u8) -> Self {
        match b {
            0 => FontSize::Small,
            2 => FontSize::Large,
            _ => FontSize::Medium,
        }
    }
}

// reader typeface: proportional sans (Helvetica), proportional serif (New
// Century Schoolbook), or monospace. see `pages::reader`.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum FontFamily {
    Sans,
    Serif,
    Mono,
}

impl FontFamily {
    pub(crate) fn next(self) -> Self {
        match self {
            FontFamily::Sans => FontFamily::Serif,
            FontFamily::Serif => FontFamily::Mono,
            FontFamily::Mono => FontFamily::Sans,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            FontFamily::Sans => "Sans",
            FontFamily::Serif => "Serif",
            FontFamily::Mono => "Mono",
        }
    }

    fn to_byte(self) -> u8 {
        match self {
            FontFamily::Sans => 0,
            FontFamily::Serif => 1,
            FontFamily::Mono => 2,
        }
    }

    fn from_byte(b: u8) -> Self {
        match b {
            1 => FontFamily::Serif,
            2 => FontFamily::Mono,
            _ => FontFamily::Sans,
        }
    }
}

// reader line spacing (leading), scaling the per-size line height.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum LineSpacing {
    Compact,
    Normal,
    Relaxed,
}

impl LineSpacing {
    pub(crate) fn next(self) -> Self {
        match self {
            LineSpacing::Compact => LineSpacing::Normal,
            LineSpacing::Normal => LineSpacing::Relaxed,
            LineSpacing::Relaxed => LineSpacing::Compact,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            LineSpacing::Compact => "Compact",
            LineSpacing::Normal => "Normal",
            LineSpacing::Relaxed => "Relaxed",
        }
    }

    fn to_byte(self) -> u8 {
        match self {
            LineSpacing::Compact => 0,
            LineSpacing::Normal => 1,
            LineSpacing::Relaxed => 2,
        }
    }

    fn from_byte(b: u8) -> Self {
        match b {
            0 => LineSpacing::Compact,
            2 => LineSpacing::Relaxed,
            _ => LineSpacing::Normal,
        }
    }
}

// home-screen icon set: thin-line Lucide or solid-filled Material. see
// `pages::home`, where each maps to a directory of BMP glyphs.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum IconStyle {
    Lucide,
    Material,
}

impl IconStyle {
    pub(crate) fn next(self) -> Self {
        match self {
            IconStyle::Lucide => IconStyle::Material,
            IconStyle::Material => IconStyle::Lucide,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            IconStyle::Lucide => "Lucide",
            IconStyle::Material => "Material",
        }
    }

    fn to_byte(self) -> u8 {
        match self {
            IconStyle::Lucide => 0,
            IconStyle::Material => 1,
        }
    }

    fn from_byte(b: u8) -> Self {
        match b {
            1 => IconStyle::Material,
            _ => IconStyle::Lucide,
        }
    }
}

// home-screen icon size: each maps to a directory of pre-rendered glyphs at
// that pixel size. see `pages::home`.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum IconSize {
    Small,
    Regular,
}

impl IconSize {
    pub(crate) fn next(self) -> Self {
        match self {
            IconSize::Small => IconSize::Regular,
            IconSize::Regular => IconSize::Small,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            IconSize::Small => "Small",
            IconSize::Regular => "Regular",
        }
    }

    fn to_byte(self) -> u8 {
        match self {
            IconSize::Small => 0,
            IconSize::Regular => 1,
        }
    }

    fn from_byte(b: u8) -> Self {
        match b {
            0 => IconSize::Small,
            _ => IconSize::Regular,
        }
    }
}

// the reader's text styling, bundled so it can be passed in one argument.
#[derive(Clone, Copy)]
pub(crate) struct ReaderStyle {
    pub(crate) size: FontSize,
    pub(crate) family: FontFamily,
    pub(crate) spacing: LineSpacing,
}

// timezone offset (hours from UTC) baked in at build time from the
// TZ_OFFSET_HOURS env (see .env). used only as the first-boot default before
// the user has saved their own offset to flash.
const DEFAULT_TZ_OFFSET: i8 = match option_env!("TZ_OFFSET_HOURS") {
    Some(s) => match konst_parse_i8(s) {
        Some(v) => v,
        None => -7,
    },
    None => -7,
};

// minimal const i8 parser so the build-time default can come from an env
// string.
const fn konst_parse_i8(s: &str) -> Option<i8> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let (neg, start) = match bytes[0] {
        b'-' => (true, 1),
        b'+' => (false, 1),
        _ => (false, 0),
    };
    if start >= bytes.len() {
        return None;
    }
    let mut acc: i32 = 0;
    let mut i = start;
    while i < bytes.len() {
        let d = bytes[i];
        if d < b'0' || d > b'9' {
            return None;
        }
        acc = acc * 10 + (d - b'0') as i32;
        i += 1;
    }
    if neg {
        acc = -acc;
    }
    if acc < -12 || acc > 14 {
        return None;
    }
    Some(acc as i8)
}

// wifi credentials baked in at build time from the SSID/PASSWORD env (see
// .env), used only as the first-boot default before the user has joined a
// network from the wifi settings page and saved their own credentials to flash.
const DEFAULT_SSID: &str = match option_env!("SSID") {
    Some(s) => s,
    None => "",
};
const DEFAULT_PASSWORD: &str = match option_env!("PASSWORD") {
    Some(s) => s,
    None => "",
};

// wifi credential capacities: a 32-byte SSID and a 63-byte WPA passphrase are
// the 802.11 maxima; the password array carries one extra byte for a round
// size.
const SSID_CAP: usize = 32;
const PASSWORD_CAP: usize = 64;

// copy `src` into `dst`, truncating to the array's capacity, and return the
// number of bytes written.
fn copy_str(dst: &mut [u8], src: &str) -> u8 {
    let n = src.len().min(dst.len());
    dst[..n].copy_from_slice(&src.as_bytes()[..n]);
    n as u8
}

#[derive(Clone, Copy)]
pub(crate) struct Settings {
    pub(crate) tz_offset_hours: i8,
    pub(crate) time_24h: bool,
    pub(crate) brightness: u8,
    pub(crate) reader_font_size: FontSize,
    pub(crate) reader_font_family: FontFamily,
    pub(crate) reader_line_spacing: LineSpacing,
    pub(crate) icon_style: IconStyle,
    pub(crate) icon_size: IconSize,
    wifi_ssid: [u8; SSID_CAP],
    wifi_ssid_len: u8,
    wifi_password: [u8; PASSWORD_CAP],
    wifi_password_len: u8,
}

impl Default for Settings {
    fn default() -> Self {
        let mut wifi_ssid = [0u8; SSID_CAP];
        let wifi_ssid_len = copy_str(&mut wifi_ssid, DEFAULT_SSID);
        let mut wifi_password = [0u8; PASSWORD_CAP];
        let wifi_password_len = copy_str(&mut wifi_password, DEFAULT_PASSWORD);
        Self {
            tz_offset_hours: DEFAULT_TZ_OFFSET,
            time_24h: true,
            brightness: 0,
            reader_font_size: FontSize::Medium,
            reader_font_family: FontFamily::Sans,
            reader_line_spacing: LineSpacing::Normal,
            icon_style: IconStyle::Lucide,
            icon_size: IconSize::Regular,
            wifi_ssid,
            wifi_ssid_len,
            wifi_password,
            wifi_password_len,
        }
    }
}

// on-flash layout: a 2-byte magic, a version, the fields in order, and an xor
// checksum over the preceding bytes. anything that doesn't validate (blank
// flash, older/newer layout, corruption) falls back to defaults.
const MAGIC: [u8; 2] = [0x54, 0x35];
const VERSION: u8 = 5;
// 11 scalar bytes, then the SSID (len + 32) and password (len + 64), then a
// trailing xor checksum.
const SSID_OFF: usize = 12;
const PASSWORD_LEN_OFF: usize = SSID_OFF + SSID_CAP;
const PASSWORD_OFF: usize = PASSWORD_LEN_OFF + 1;
const CHECKSUM_OFF: usize = PASSWORD_OFF + PASSWORD_CAP;
const BLOB_LEN: usize = CHECKSUM_OFF + 1;

// the flash peripheral is a singleton held by `esp_hal::init`; settings access
// is brief and self-contained, so steal it here the same way the SD card and
// radio paths steal their shared buses.
fn flash() -> FlashStorage<'static> {
    FlashStorage::new(unsafe { esp_hal::peripherals::FLASH::steal() })
}

impl Settings {
    fn encode(&self) -> [u8; BLOB_LEN] {
        let mut buf = [0u8; BLOB_LEN];
        buf[0] = MAGIC[0];
        buf[1] = MAGIC[1];
        buf[2] = VERSION;
        buf[3] = self.tz_offset_hours as u8;
        buf[4] = u8::from(self.time_24h);
        buf[5] = self.brightness.min(100);
        buf[6] = self.reader_font_size.to_byte();
        buf[7] = self.reader_font_family.to_byte();
        buf[8] = self.reader_line_spacing.to_byte();
        buf[9] = self.icon_style.to_byte();
        buf[10] = self.icon_size.to_byte();
        buf[11] = self.wifi_ssid_len.min(SSID_CAP as u8);
        buf[SSID_OFF..PASSWORD_LEN_OFF].copy_from_slice(&self.wifi_ssid);
        buf[PASSWORD_LEN_OFF] = self.wifi_password_len.min(PASSWORD_CAP as u8);
        buf[PASSWORD_OFF..CHECKSUM_OFF].copy_from_slice(&self.wifi_password);
        buf[CHECKSUM_OFF] = buf[0..CHECKSUM_OFF].iter().fold(0u8, |acc, &b| acc ^ b);
        buf
    }

    fn decode(buf: &[u8; BLOB_LEN]) -> Option<Self> {
        if buf[0..2] != MAGIC || buf[2] != VERSION {
            return None;
        }
        let checksum = buf[0..CHECKSUM_OFF].iter().fold(0u8, |acc, &b| acc ^ b);
        if checksum != buf[CHECKSUM_OFF] {
            return None;
        }
        let mut wifi_ssid = [0u8; SSID_CAP];
        wifi_ssid.copy_from_slice(&buf[SSID_OFF..PASSWORD_LEN_OFF]);
        let mut wifi_password = [0u8; PASSWORD_CAP];
        wifi_password.copy_from_slice(&buf[PASSWORD_OFF..CHECKSUM_OFF]);
        Some(Self {
            tz_offset_hours: buf[3] as i8,
            time_24h: buf[4] != 0,
            brightness: buf[5].min(100),
            reader_font_size: FontSize::from_byte(buf[6]),
            reader_font_family: FontFamily::from_byte(buf[7]),
            reader_line_spacing: LineSpacing::from_byte(buf[8]),
            icon_style: IconStyle::from_byte(buf[9]),
            icon_size: IconSize::from_byte(buf[10]),
            wifi_ssid,
            wifi_ssid_len: buf[11].min(SSID_CAP as u8),
            wifi_password,
            wifi_password_len: buf[PASSWORD_LEN_OFF].min(PASSWORD_CAP as u8),
        })
    }

    // the saved SSID as a string, or "" if none is configured (or the stored
    // bytes are not valid utf-8).
    pub(crate) fn wifi_ssid(&self) -> &str {
        let len = (self.wifi_ssid_len as usize).min(SSID_CAP);
        core::str::from_utf8(&self.wifi_ssid[..len]).unwrap_or("")
    }

    pub(crate) fn wifi_password(&self) -> &str {
        let len = (self.wifi_password_len as usize).min(PASSWORD_CAP);
        core::str::from_utf8(&self.wifi_password[..len]).unwrap_or("")
    }

    // replace the saved wifi credentials, truncating to the field capacities.
    pub(crate) fn set_wifi(&mut self, ssid: &str, password: &str) {
        self.wifi_ssid = [0u8; SSID_CAP];
        self.wifi_ssid_len = copy_str(&mut self.wifi_ssid, ssid);
        self.wifi_password = [0u8; PASSWORD_CAP];
        self.wifi_password_len = copy_str(&mut self.wifi_password, password);
    }

    pub(crate) fn reader_style(&self) -> ReaderStyle {
        ReaderStyle {
            size: self.reader_font_size,
            family: self.reader_font_family,
            spacing: self.reader_line_spacing,
        }
    }

    // read the saved settings from the NVS data partition, falling back to
    // defaults when the partition is missing or holds no valid blob.
    pub(crate) fn load() -> Self {
        let mut flash = flash();
        let mut table_buf = vec![0u8; PARTITION_TABLE_MAX_LEN];
        let table = match read_partition_table(&mut flash, &mut table_buf) {
            Ok(table) => table,
            Err(e) => {
                esp_println::println!("settings: read partition table failed: {e:?}");
                return Self::default();
            }
        };
        let entry = match table.find_partition(PartitionType::Data(DataPartitionSubType::Nvs)) {
            Ok(Some(entry)) => entry,
            Ok(None) => {
                esp_println::println!("settings: no nvs partition; using defaults");
                return Self::default();
            }
            Err(e) => {
                esp_println::println!("settings: find nvs partition failed: {e:?}");
                return Self::default();
            }
        };
        let mut region = entry.as_embedded_storage(&mut flash);
        let mut buf = [0u8; BLOB_LEN];
        if let Err(e) = region.read(0, &mut buf) {
            esp_println::println!("settings: read failed: {e:?}");
            return Self::default();
        }
        Self::decode(&buf).unwrap_or_default()
    }

    // persist the settings to the NVS data partition. best effort: logs and
    // returns on any failure, matching the reader's progress-save behaviour.
    pub(crate) fn save(&self) {
        let mut flash = flash();
        let mut table_buf = vec![0u8; PARTITION_TABLE_MAX_LEN];
        let table = match read_partition_table(&mut flash, &mut table_buf) {
            Ok(table) => table,
            Err(e) => {
                esp_println::println!("settings: read partition table failed: {e:?}");
                return;
            }
        };
        let entry = match table.find_partition(PartitionType::Data(DataPartitionSubType::Nvs)) {
            Ok(Some(entry)) => entry,
            _ => {
                esp_println::println!("settings: no nvs partition; not saving");
                return;
            }
        };
        let mut region = entry.as_embedded_storage(&mut flash);
        if let Err(e) = region.write(0, &self.encode()) {
            esp_println::println!("settings: write failed: {e:?}");
        }
    }
}
