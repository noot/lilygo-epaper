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

// IO48 auxiliary button behavior when pressed.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Io48Action {
    Sleep,
    Backlight,
    LoraReceive,
    Nothing,
}

impl Io48Action {
    pub(crate) fn next(self) -> Self {
        match self {
            Io48Action::Sleep => Io48Action::Backlight,
            Io48Action::Backlight => Io48Action::LoraReceive,
            Io48Action::LoraReceive => Io48Action::Nothing,
            Io48Action::Nothing => Io48Action::Sleep,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Io48Action::Sleep => "Sleep",
            Io48Action::Backlight => "Backlight",
            Io48Action::LoraReceive => "LoRa RX",
            Io48Action::Nothing => "Nothing",
        }
    }

    fn to_byte(self) -> u8 {
        match self {
            Io48Action::Sleep => 0,
            Io48Action::Backlight => 1,
            Io48Action::LoraReceive => 2,
            Io48Action::Nothing => 3,
        }
    }

    fn from_byte(b: u8) -> Self {
        match b {
            1 => Io48Action::Backlight,
            2 => Io48Action::LoraReceive,
            3 => Io48Action::Nothing,
            _ => Io48Action::Sleep,
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
// how many joined networks are remembered (most recently used first), so
// switching between e.g. home wifi and a phone hotspot never re-prompts for a
// passphrase.
const WIFI_NETWORK_CAP: usize = 5;

// copy `src` into `dst`, truncating to the array's capacity, and return the
// number of bytes written.
fn copy_str(dst: &mut [u8], src: &str) -> u8 {
    let n = src.len().min(dst.len());
    dst[..n].copy_from_slice(&src.as_bytes()[..n]);
    n as u8
}

// one remembered wifi network: fixed-capacity ssid + passphrase so `Settings`
// stays `Copy`.
#[derive(Clone, Copy)]
struct WifiNetwork {
    ssid: [u8; SSID_CAP],
    ssid_len: u8,
    password: [u8; PASSWORD_CAP],
    password_len: u8,
}

impl WifiNetwork {
    const EMPTY: Self = Self {
        ssid: [0; SSID_CAP],
        ssid_len: 0,
        password: [0; PASSWORD_CAP],
        password_len: 0,
    };

    fn new(ssid: &str, password: &str) -> Self {
        let mut net = Self::EMPTY;
        net.ssid_len = copy_str(&mut net.ssid, ssid);
        net.password_len = copy_str(&mut net.password, password);
        net
    }

    fn ssid(&self) -> &str {
        let len = (self.ssid_len as usize).min(SSID_CAP);
        core::str::from_utf8(&self.ssid[..len]).unwrap_or("")
    }

    fn password(&self) -> &str {
        let len = (self.password_len as usize).min(PASSWORD_CAP);
        core::str::from_utf8(&self.password[..len]).unwrap_or("")
    }
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
    pub(crate) io48_action: Io48Action,
    /// keep the lora radio and mesh membership alive on every screen (at a
    /// standing rx current cost), instead of only while the lora page is open.
    pub(crate) mesh_background: bool,
    /// mesh display name, flooded as an alias claim; empty = id only.
    mesh_alias: [u8; ALIAS_CAP],
    mesh_alias_len: u8,
    wifi_networks: [WifiNetwork; WIFI_NETWORK_CAP],
    wifi_network_count: u8,
}

impl Default for Settings {
    fn default() -> Self {
        let mut wifi_networks = [WifiNetwork::EMPTY; WIFI_NETWORK_CAP];
        let mut wifi_network_count = 0;
        if !DEFAULT_SSID.is_empty() {
            wifi_networks[0] = WifiNetwork::new(DEFAULT_SSID, DEFAULT_PASSWORD);
            wifi_network_count = 1;
        }
        Self {
            tz_offset_hours: DEFAULT_TZ_OFFSET,
            time_24h: true,
            brightness: 0,
            reader_font_size: FontSize::Medium,
            reader_font_family: FontFamily::Sans,
            reader_line_spacing: LineSpacing::Normal,
            icon_style: IconStyle::Lucide,
            icon_size: IconSize::Regular,
            io48_action: Io48Action::Sleep,
            mesh_background: false,
            mesh_alias: [0; ALIAS_CAP],
            mesh_alias_len: 0,
            wifi_networks,
            wifi_network_count,
        }
    }
}

// on-flash layout: a 2-byte magic, a version, the fields in order, and an xor
// checksum over the preceding bytes. anything that doesn't validate (blank
// flash, older/newer layout, corruption) falls back to defaults, except the
// immediately previous version which is migrated (see `decode_v5`).
const MAGIC: [u8; 2] = [0x54, 0x35];
const VERSION: u8 = 9;
/// mesh display-name capacity, matching nootmesh's wire cap.
pub(crate) const ALIAS_CAP: usize = 12;
// 13 scalar bytes (added io48_action at byte 12), an alias (len + 12), a
// saved-network count, then WIFI_NETWORK_CAP fixed-size network entries (ssid
// len + 32, password len + 64), then a trailing xor checksum.
const ALIAS_OFF: usize = 13;
const NETWORKS_OFF: usize = ALIAS_OFF + 1 + ALIAS_CAP + 1;
const NETWORK_SIZE: usize = 1 + SSID_CAP + 1 + PASSWORD_CAP;
const CHECKSUM_OFF: usize = NETWORKS_OFF + WIFI_NETWORK_CAP * NETWORK_SIZE;
const BLOB_LEN: usize = CHECKSUM_OFF + 1;

// the version-8 layout: identical except it lacked the io48_action field, so
// the alias offset was at 12 instead of 13.
const V8_VERSION: u8 = 8;
const V8_ALIAS_OFF: usize = 12;
const V8_NETWORKS_OFF: usize = V8_ALIAS_OFF + 1 + ALIAS_CAP + 1;
const V8_CHECKSUM_OFF: usize = V8_NETWORKS_OFF + WIFI_NETWORK_CAP * NETWORK_SIZE;

// the version-7 layout: identical except it lacked the alias field, so the
// network table sat 13 bytes earlier.
const V7_VERSION: u8 = 7;
const V7_NETWORKS_OFF: usize = 13;
const V7_CHECKSUM_OFF: usize = V7_NETWORKS_OFF + WIFI_NETWORK_CAP * NETWORK_SIZE;

// the version-6 layout: no mesh-background byte either.
const V6_VERSION: u8 = 6;
const V6_NETWORKS_OFF: usize = 12;
const V6_CHECKSUM_OFF: usize = V6_NETWORKS_OFF + WIFI_NETWORK_CAP * NETWORK_SIZE;

// the version-5 single-network layout, kept so an upgraded firmware migrates
// the previously saved settings instead of dropping them.
const V5_VERSION: u8 = 5;
const V5_SSID_OFF: usize = 12;
const V5_PASSWORD_LEN_OFF: usize = V5_SSID_OFF + SSID_CAP;
const V5_PASSWORD_OFF: usize = V5_PASSWORD_LEN_OFF + 1;
const V5_CHECKSUM_OFF: usize = V5_PASSWORD_OFF + PASSWORD_CAP;

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
        buf[11] = u8::from(self.mesh_background);
        buf[12] = self.io48_action.to_byte();
        buf[ALIAS_OFF] = self.mesh_alias_len.min(ALIAS_CAP as u8);
        buf[ALIAS_OFF + 1..ALIAS_OFF + 1 + ALIAS_CAP].copy_from_slice(&self.mesh_alias);
        buf[NETWORKS_OFF - 1] = self.wifi_network_count.min(WIFI_NETWORK_CAP as u8);
        for (i, net) in self.wifi_networks.iter().enumerate() {
            let off = NETWORKS_OFF + i * NETWORK_SIZE;
            buf[off] = net.ssid_len.min(SSID_CAP as u8);
            buf[off + 1..off + 1 + SSID_CAP].copy_from_slice(&net.ssid);
            buf[off + 1 + SSID_CAP] = net.password_len.min(PASSWORD_CAP as u8);
            buf[off + 2 + SSID_CAP..off + NETWORK_SIZE].copy_from_slice(&net.password);
        }
        buf[CHECKSUM_OFF] = buf[0..CHECKSUM_OFF].iter().fold(0u8, |acc, &b| acc ^ b);
        buf
    }

    fn decode(buf: &[u8; BLOB_LEN]) -> Option<Self> {
        if buf[0..2] != MAGIC {
            return None;
        }
        if buf[2] == V5_VERSION {
            return Self::decode_v5(buf);
        }
        if buf[2] == V8_VERSION {
            return Self::decode_v8(buf);
        }
        if buf[2] == V7_VERSION {
            return Self::decode_v7(buf);
        }
        if buf[2] == V6_VERSION {
            return Self::decode_v6(buf);
        }
        if buf[2] != VERSION {
            return None;
        }
        let checksum = buf[0..CHECKSUM_OFF].iter().fold(0u8, |acc, &b| acc ^ b);
        if checksum != buf[CHECKSUM_OFF] {
            return None;
        }
        let mut wifi_networks = [WifiNetwork::EMPTY; WIFI_NETWORK_CAP];
        for (i, net) in wifi_networks.iter_mut().enumerate() {
            let off = NETWORKS_OFF + i * NETWORK_SIZE;
            net.ssid_len = buf[off].min(SSID_CAP as u8);
            net.ssid.copy_from_slice(&buf[off + 1..off + 1 + SSID_CAP]);
            net.password_len = buf[off + 1 + SSID_CAP].min(PASSWORD_CAP as u8);
            net.password
                .copy_from_slice(&buf[off + 2 + SSID_CAP..off + NETWORK_SIZE]);
        }
        Some(Self {
            tz_offset_hours: buf[3] as i8,
            time_24h: buf[4] != 0,
            brightness: buf[5].min(100),
            reader_font_size: FontSize::from_byte(buf[6]),
            reader_font_family: FontFamily::from_byte(buf[7]),
            reader_line_spacing: LineSpacing::from_byte(buf[8]),
            icon_style: IconStyle::from_byte(buf[9]),
            icon_size: IconSize::from_byte(buf[10]),
            mesh_background: buf[11] != 0,
            io48_action: Io48Action::from_byte(buf[12]),
            mesh_alias: {
                let mut alias = [0; ALIAS_CAP];
                alias.copy_from_slice(&buf[ALIAS_OFF + 1..ALIAS_OFF + 1 + ALIAS_CAP]);
                alias
            },
            mesh_alias_len: buf[ALIAS_OFF].min(ALIAS_CAP as u8),
            wifi_networks,
            wifi_network_count: buf[NETWORKS_OFF - 1].min(WIFI_NETWORK_CAP as u8),
        })
    }

    // migrate a version-8 blob: identical scalars, no io48_action (defaults to
    // Sleep), alias offset at 12 instead of 13.
    fn decode_v8(buf: &[u8; BLOB_LEN]) -> Option<Self> {
        let checksum = buf[0..V8_CHECKSUM_OFF].iter().fold(0u8, |acc, &b| acc ^ b);
        if checksum != buf[V8_CHECKSUM_OFF] {
            return None;
        }
        let mut wifi_networks = [WifiNetwork::EMPTY; WIFI_NETWORK_CAP];
        for (i, net) in wifi_networks.iter_mut().enumerate() {
            let off = V8_NETWORKS_OFF + i * NETWORK_SIZE;
            net.ssid_len = buf[off].min(SSID_CAP as u8);
            net.ssid.copy_from_slice(&buf[off + 1..off + 1 + SSID_CAP]);
            net.password_len = buf[off + 1 + SSID_CAP].min(PASSWORD_CAP as u8);
            net.password
                .copy_from_slice(&buf[off + 2 + SSID_CAP..off + NETWORK_SIZE]);
        }
        Some(Self {
            tz_offset_hours: buf[3] as i8,
            time_24h: buf[4] != 0,
            brightness: buf[5].min(100),
            reader_font_size: FontSize::from_byte(buf[6]),
            reader_font_family: FontFamily::from_byte(buf[7]),
            reader_line_spacing: LineSpacing::from_byte(buf[8]),
            icon_style: IconStyle::from_byte(buf[9]),
            icon_size: IconSize::from_byte(buf[10]),
            mesh_background: buf[11] != 0,
            io48_action: Io48Action::Sleep,
            mesh_alias: {
                let mut alias = [0; ALIAS_CAP];
                alias.copy_from_slice(&buf[V8_ALIAS_OFF + 1..V8_ALIAS_OFF + 1 + ALIAS_CAP]);
                alias
            },
            mesh_alias_len: buf[V8_ALIAS_OFF].min(ALIAS_CAP as u8),
            wifi_networks,
            wifi_network_count: buf[V8_NETWORKS_OFF - 1].min(WIFI_NETWORK_CAP as u8),
        })
    }

    // migrate a version-7 blob: identical scalars, no alias (empty), network
    // table 13 bytes earlier, no io48_action (defaults to Sleep).
    fn decode_v7(buf: &[u8; BLOB_LEN]) -> Option<Self> {
        let checksum = buf[0..V7_CHECKSUM_OFF].iter().fold(0u8, |acc, &b| acc ^ b);
        if checksum != buf[V7_CHECKSUM_OFF] {
            return None;
        }
        let mut wifi_networks = [WifiNetwork::EMPTY; WIFI_NETWORK_CAP];
        for (i, net) in wifi_networks.iter_mut().enumerate() {
            let off = V7_NETWORKS_OFF + i * NETWORK_SIZE;
            net.ssid_len = buf[off].min(SSID_CAP as u8);
            net.ssid.copy_from_slice(&buf[off + 1..off + 1 + SSID_CAP]);
            net.password_len = buf[off + 1 + SSID_CAP].min(PASSWORD_CAP as u8);
            net.password
                .copy_from_slice(&buf[off + 2 + SSID_CAP..off + NETWORK_SIZE]);
        }
        Some(Self {
            tz_offset_hours: buf[3] as i8,
            time_24h: buf[4] != 0,
            brightness: buf[5].min(100),
            reader_font_size: FontSize::from_byte(buf[6]),
            reader_font_family: FontFamily::from_byte(buf[7]),
            reader_line_spacing: LineSpacing::from_byte(buf[8]),
            icon_style: IconStyle::from_byte(buf[9]),
            icon_size: IconSize::from_byte(buf[10]),
            mesh_background: buf[11] != 0,
            io48_action: Io48Action::Sleep,
            mesh_alias: [0; ALIAS_CAP],
            mesh_alias_len: 0,
            wifi_networks,
            wifi_network_count: buf[12].min(WIFI_NETWORK_CAP as u8),
        })
    }

    // migrate a version-6 blob: identical scalars, network table one byte
    // earlier, and no mesh-background flag (defaults off), no io48_action.
    fn decode_v6(buf: &[u8; BLOB_LEN]) -> Option<Self> {
        let checksum = buf[0..V6_CHECKSUM_OFF].iter().fold(0u8, |acc, &b| acc ^ b);
        if checksum != buf[V6_CHECKSUM_OFF] {
            return None;
        }
        let mut wifi_networks = [WifiNetwork::EMPTY; WIFI_NETWORK_CAP];
        for (i, net) in wifi_networks.iter_mut().enumerate() {
            let off = V6_NETWORKS_OFF + i * NETWORK_SIZE;
            net.ssid_len = buf[off].min(SSID_CAP as u8);
            net.ssid.copy_from_slice(&buf[off + 1..off + 1 + SSID_CAP]);
            net.password_len = buf[off + 1 + SSID_CAP].min(PASSWORD_CAP as u8);
            net.password
                .copy_from_slice(&buf[off + 2 + SSID_CAP..off + NETWORK_SIZE]);
        }
        Some(Self {
            tz_offset_hours: buf[3] as i8,
            time_24h: buf[4] != 0,
            brightness: buf[5].min(100),
            reader_font_size: FontSize::from_byte(buf[6]),
            reader_font_family: FontFamily::from_byte(buf[7]),
            reader_line_spacing: LineSpacing::from_byte(buf[8]),
            icon_style: IconStyle::from_byte(buf[9]),
            icon_size: IconSize::from_byte(buf[10]),
            mesh_background: false,
            io48_action: Io48Action::Sleep,
            mesh_alias: [0; ALIAS_CAP],
            mesh_alias_len: 0,
            wifi_networks,
            wifi_network_count: buf[11].min(WIFI_NETWORK_CAP as u8),
        })
    }

    // migrate a version-5 blob: identical scalars, and its single stored
    // network becomes the first (most recently used) table entry, no io48_action.
    fn decode_v5(buf: &[u8; BLOB_LEN]) -> Option<Self> {
        let checksum = buf[0..V5_CHECKSUM_OFF].iter().fold(0u8, |acc, &b| acc ^ b);
        if checksum != buf[V5_CHECKSUM_OFF] {
            return None;
        }
        let mut net = WifiNetwork::EMPTY;
        net.ssid_len = buf[11].min(SSID_CAP as u8);
        net.ssid
            .copy_from_slice(&buf[V5_SSID_OFF..V5_PASSWORD_LEN_OFF]);
        net.password_len = buf[V5_PASSWORD_LEN_OFF].min(PASSWORD_CAP as u8);
        net.password
            .copy_from_slice(&buf[V5_PASSWORD_OFF..V5_CHECKSUM_OFF]);
        let mut wifi_networks = [WifiNetwork::EMPTY; WIFI_NETWORK_CAP];
        let wifi_network_count = u8::from(net.ssid_len > 0);
        wifi_networks[0] = net;
        Some(Self {
            tz_offset_hours: buf[3] as i8,
            time_24h: buf[4] != 0,
            brightness: buf[5].min(100),
            reader_font_size: FontSize::from_byte(buf[6]),
            reader_font_family: FontFamily::from_byte(buf[7]),
            reader_line_spacing: LineSpacing::from_byte(buf[8]),
            icon_style: IconStyle::from_byte(buf[9]),
            icon_size: IconSize::from_byte(buf[10]),
            mesh_background: false,
            io48_action: Io48Action::Sleep,
            mesh_alias: [0; ALIAS_CAP],
            mesh_alias_len: 0,
            wifi_networks,
            wifi_network_count,
        })
    }

    // the mesh display name, or "" when unset (id-only display).
    pub(crate) fn mesh_alias(&self) -> &str {
        let len = (self.mesh_alias_len as usize).min(ALIAS_CAP);
        core::str::from_utf8(&self.mesh_alias[..len]).unwrap_or("")
    }

    pub(crate) fn set_mesh_alias(&mut self, name: &str) {
        let bytes = name.as_bytes();
        let len = bytes.len().min(ALIAS_CAP);
        self.mesh_alias = [0; ALIAS_CAP];
        self.mesh_alias[..len].copy_from_slice(&bytes[..len]);
        self.mesh_alias_len = len as u8;
    }

    // the most recently used network's SSID, or "" if none is saved (or the
    // stored bytes are not valid utf-8).
    pub(crate) fn wifi_ssid(&self) -> &str {
        if self.wifi_network_count == 0 {
            ""
        } else {
            self.wifi_networks[0].ssid()
        }
    }

    pub(crate) fn wifi_password(&self) -> &str {
        if self.wifi_network_count == 0 {
            ""
        } else {
            self.wifi_networks[0].password()
        }
    }

    // the stored passphrase for `ssid`, if that network was joined before.
    pub(crate) fn saved_wifi_password(&self, ssid: &str) -> Option<&str> {
        let count = (self.wifi_network_count as usize).min(WIFI_NETWORK_CAP);
        self.wifi_networks[..count]
            .iter()
            .find(|net| net.ssid() == ssid)
            .map(|net| net.password())
    }

    // save credentials as the most recently used network, truncating to the
    // field capacities. an existing entry for the ssid is updated and moved to
    // the front; otherwise the entry is inserted at the front, evicting the
    // least recently used network when the table is full.
    pub(crate) fn set_wifi(&mut self, ssid: &str, password: &str) {
        let count = (self.wifi_network_count as usize).min(WIFI_NETWORK_CAP);
        let (vacated, new_count) = match self.wifi_networks[..count]
            .iter()
            .position(|net| net.ssid() == ssid)
        {
            Some(i) => (i, count),
            None => (
                count.min(WIFI_NETWORK_CAP - 1),
                (count + 1).min(WIFI_NETWORK_CAP),
            ),
        };
        // shift the entries above the vacated slot down one, then write the
        // new entry at the front.
        for i in (1..=vacated).rev() {
            self.wifi_networks[i] = self.wifi_networks[i - 1];
        }
        self.wifi_networks[0] = WifiNetwork::new(ssid, password);
        self.wifi_network_count = new_count as u8;
    }

    // drop the saved entry for `ssid`, if any, shifting later entries up.
    pub(crate) fn forget_wifi(&mut self, ssid: &str) {
        let count = (self.wifi_network_count as usize).min(WIFI_NETWORK_CAP);
        let Some(pos) = self.wifi_networks[..count]
            .iter()
            .position(|net| net.ssid() == ssid)
        else {
            return;
        };
        for i in pos..count - 1 {
            self.wifi_networks[i] = self.wifi_networks[i + 1];
        }
        self.wifi_networks[count - 1] = WifiNetwork::EMPTY;
        self.wifi_network_count = (count - 1) as u8;
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
