//! SSD1680 command opcodes (used to drive the DEPG0213BN panel).

pub(crate) const SW_RESET: u8 = 0x12;
pub(crate) const DRIVER_OUTPUT_CONTROL: u8 = 0x01;
pub(crate) const DATA_ENTRY_MODE: u8 = 0x11;
pub(crate) const BORDER_WAVEFORM: u8 = 0x3C;
pub(crate) const DISPLAY_UPDATE_CONTROL_1: u8 = 0x21;
pub(crate) const TEMP_SENSOR_CONTROL: u8 = 0x18;
pub(crate) const SET_RAM_X_RANGE: u8 = 0x44;
pub(crate) const SET_RAM_Y_RANGE: u8 = 0x45;
pub(crate) const SET_RAM_X_COUNTER: u8 = 0x4E;
pub(crate) const SET_RAM_Y_COUNTER: u8 = 0x4F;
pub(crate) const WRITE_RAM_BW: u8 = 0x24;
pub(crate) const WRITE_RAM_RED: u8 = 0x26;
pub(crate) const WRITE_LUT: u8 = 0x32;
pub(crate) const DISPLAY_UPDATE_CONTROL_2: u8 = 0x22;
pub(crate) const MASTER_ACTIVATION: u8 = 0x20;
pub(crate) const DEEP_SLEEP: u8 = 0x10;

// display update control 2 (0x22) sequences, run by master activation (0x20).
pub(crate) const UPDATE_SEQUENCE_FULL: u8 = 0xF7;
pub(crate) const UPDATE_SEQUENCE_PARTIAL: u8 = 0xCC;
