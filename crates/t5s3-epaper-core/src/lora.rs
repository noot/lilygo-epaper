use core::cell::RefCell;

use embedded_hal::spi::SpiDevice as _;
use esp_hal::{
    delay::Delay,
    gpio::{Input, InputConfig, Level, Output, OutputConfig, RtcPin},
    peripherals,
    spi::{
        master::{Config as SpiConfig, ConfigError as SpiConfigError, Spi},
        Mode as SpiMode,
    },
    time::Rate,
    Blocking,
};
use log::debug;

use crate::bus::SharedSpiDevice;

const SPI_FREQUENCY_KHZ: u32 = 2000;
const BUSY_TIMEOUT_ITERS: u32 = 1000;
const BUSY_POLL_INTERVAL_US: u32 = 100;
const TX_TIMEOUT_ITERS: u32 = 5000;
const MAX_PAYLOAD: usize = 255;
const XTAL_FREQUENCY_HZ: u64 = 32_000_000;

const OP_SET_STANDBY: u8 = 0x80;
const OP_SET_TX: u8 = 0x83;
const OP_SET_RX: u8 = 0x82;
const OP_SET_RF_FREQUENCY: u8 = 0x86;
const OP_CALIBRATE_IMAGE: u8 = 0x98;
const OP_CALIBRATE: u8 = 0x89;
const OP_SET_PACKET_TYPE: u8 = 0x8A;
const OP_SET_MODULATION_PARAMS: u8 = 0x8B;
const OP_SET_PACKET_PARAMS: u8 = 0x8C;
const OP_SET_TX_PARAMS: u8 = 0x8E;
const OP_SET_PA_CONFIG: u8 = 0x95;
const OP_SET_DIO3_AS_TCXO_CTRL: u8 = 0x97;
const OP_SET_DIO2_AS_RF_SWITCH: u8 = 0x9D;
const OP_SET_BUFFER_BASE_ADDRESS: u8 = 0x8F;
const OP_SET_DIO_IRQ_PARAMS: u8 = 0x08;
const OP_WRITE_BUFFER: u8 = 0x0E;
const OP_READ_BUFFER: u8 = 0x1E;
const OP_WRITE_REGISTER: u8 = 0x0D;
const OP_READ_REGISTER: u8 = 0x1D;
const OP_GET_STATUS: u8 = 0xC0;
const OP_GET_RX_BUFFER_STATUS: u8 = 0x13;
const OP_GET_PACKET_STATUS: u8 = 0x14;
const OP_GET_IRQ_STATUS: u8 = 0x12;
const OP_CLEAR_IRQ_STATUS: u8 = 0x02;
const OP_GET_DEVICE_ERRORS: u8 = 0x17;
const OP_CLEAR_DEVICE_ERRORS: u8 = 0x07;

const STDBY_RC: u8 = 0x00;
const PACKET_TYPE_LORA: u8 = 0x01;
const TCXO_VOLTAGE_2_4V: u8 = 0x04;
const TCXO_TIMEOUT: u32 = 320; // 320 * 15.625 us = 5 ms
const CALIBRATE_ALL: u8 = 0x7F;
const RAMP_TIME_200US: u8 = 0x04;
const OCP_140MA: u8 = 0x38;
const HEADER_EXPLICIT: u8 = 0x00;
const CRC_ON: u8 = 0x01;
const IQ_STANDARD: u8 = 0x00;
// Image calibration band for the 902-928 MHz (US/Canada) ISM band.
const IMG_CAL_902_928: [u8; 2] = [0xE1, 0xE9];

const IRQ_TX_DONE: u16 = 0x0001;
const IRQ_RX_DONE: u16 = 0x0002;
const IRQ_CRC_ERR: u16 = 0x0040;
const IRQ_TIMEOUT: u16 = 0x0200;
const IRQ_ALL: u16 = 0x03FF;

const REG_OCP: u16 = 0x08E7;
const REG_LORA_SYNC_WORD: u16 = 0x0740;
/// RadioLib's "private network" sync word as expressed on the SX126x. Matches
/// the default of a stock RadioLib peer (such as the T3-S3).
const SYNC_WORD_PRIVATE: u16 = 0x1424;

/// LoRa signal bandwidth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bandwidth {
    Bw125,
    Bw250,
    Bw500,
}

impl Bandwidth {
    fn register(self) -> u8 {
        match self {
            Self::Bw125 => 0x04,
            Self::Bw250 => 0x05,
            Self::Bw500 => 0x06,
        }
    }

    fn hz(self) -> u32 {
        match self {
            Self::Bw125 => 125_000,
            Self::Bw250 => 250_000,
            Self::Bw500 => 500_000,
        }
    }
}

/// LoRa spreading factor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpreadingFactor {
    Sf7,
    Sf8,
    Sf9,
    Sf10,
    Sf11,
    Sf12,
}

impl SpreadingFactor {
    fn register(self) -> u8 {
        match self {
            Self::Sf7 => 0x07,
            Self::Sf8 => 0x08,
            Self::Sf9 => 0x09,
            Self::Sf10 => 0x0A,
            Self::Sf11 => 0x0B,
            Self::Sf12 => 0x0C,
        }
    }
}

/// LoRa forward error correction coding rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodingRate {
    Cr4_5,
    Cr4_6,
    Cr4_7,
    Cr4_8,
}

impl CodingRate {
    fn register(self) -> u8 {
        match self {
            Self::Cr4_5 => 0x01,
            Self::Cr4_6 => 0x02,
            Self::Cr4_7 => 0x03,
            Self::Cr4_8 => 0x04,
        }
    }
}

/// Radio configuration. The defaults target the 915 MHz ISM band used in
/// North America and interoperate with a stock RadioLib peer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    pub frequency_hz: u32,
    pub bandwidth: Bandwidth,
    pub spreading_factor: SpreadingFactor,
    pub coding_rate: CodingRate,
    pub preamble_length: u16,
    pub tx_power_dbm: i8,
    pub sync_word: u16,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            frequency_hz: 915_000_000,
            bandwidth: Bandwidth::Bw125,
            spreading_factor: SpreadingFactor::Sf10,
            coding_rate: CodingRate::Cr4_5,
            preamble_length: 8,
            tx_power_dbm: 17,
            sync_word: SYNC_WORD_PRIVATE,
        }
    }
}

pub struct PinConfig<'d> {
    pub cs: peripherals::GPIO46<'d>,
    pub rst: peripherals::GPIO1<'d>,
    pub busy: peripherals::GPIO47<'d>,
    pub dio1: peripherals::GPIO10<'d>,
}

/// Semtech SX1262 LoRa transceiver driver.
///
/// Talks to the radio over the shared board SPI bus with a dedicated chip
/// select. The GPS/LoRa 3.3 V power rail (PCA9555 IO0_0) is enabled by
/// `Display::new()`, so the radio is powered for the rest of this power cycle,
/// until `Display::deep_sleep()` cuts the rail.
pub struct Lora<'a, 'd> {
    device: SharedSpiDevice<'a, 'd>,
    rst: Output<'d>,
    busy: Input<'d>,
    dio1: Input<'d>,
    delay: Delay,
    config: Config,
    last_rssi_dbm: i16,
    last_snr_db: i16,
}

impl<'a, 'd> Lora<'a, 'd> {
    /// Create a new LoRa driver and configure it for transmit/receive.
    ///
    /// Runs on the caller-owned shared SPI bus (`bus`), which must outlive the
    /// driver. Resets the radio, enables the TCXO, calibrates, and applies the
    /// RF and modulation settings from `config`, leaving the chip in standby
    /// ready to transmit.
    pub fn new(
        bus: &'a RefCell<Spi<'d, Blocking>>,
        pins: PinConfig<'d>,
        config: &Config,
    ) -> Result<Self, Error> {
        // deep sleep holds RST low to stop the unpowered SX1262 being
        // back-powered through its reset pull-up; release that hold before
        // driving the line again.
        pins.rst.rtcio_pad_hold(false);

        let device = SharedSpiDevice::new(
            bus,
            Output::new(pins.cs, Level::High, OutputConfig::default()),
            SpiConfig::default()
                .with_frequency(Rate::from_khz(SPI_FREQUENCY_KHZ))
                .with_mode(SpiMode::_0),
        );

        let mut radio = Self {
            device,
            rst: Output::new(pins.rst, Level::High, OutputConfig::default()),
            busy: Input::new(pins.busy, InputConfig::default()),
            dio1: Input::new(pins.dio1, InputConfig::default()),
            delay: Delay::new(),
            config: *config,
            last_rssi_dbm: 0,
            last_snr_db: 0,
        };

        radio.init()?;

        debug!("lora: SX1262 ready at {} Hz", config.frequency_hz);
        Ok(radio)
    }

    /// Transmit a single LoRa packet, blocking until the radio reports the
    /// transmission is complete.
    pub fn transmit(&mut self, data: &[u8]) -> Result<(), Error> {
        if data.len() > MAX_PAYLOAD {
            return Err(Error::PayloadTooLong(data.len()));
        }

        self.set_standby()?;
        self.set_packet_params(data.len() as u8)?;
        self.write_buffer(data)?;
        self.clear_irq_status(IRQ_ALL)?;
        self.set_tx()?;
        self.wait_tx_done()?;
        self.clear_irq_status(IRQ_ALL)
    }

    /// Listen for a single LoRa packet, blocking until one arrives or
    /// `timeout_ms` elapses.
    ///
    /// On success the payload is written into `buf` and the number of bytes is
    /// returned. Returns `Ok(None)` on timeout or if a packet failed its CRC.
    /// RSSI and SNR for a received packet are available via [`Lora::rssi`] and
    /// [`Lora::snr`].
    pub fn receive(&mut self, buf: &mut [u8], timeout_ms: u32) -> Result<Option<usize>, Error> {
        self.set_standby()?;
        self.set_packet_params(MAX_PAYLOAD as u8)?;
        self.clear_irq_status(IRQ_ALL)?;
        self.set_rx_continuous()?;

        let mut elapsed = 0;
        loop {
            if self.dio1.is_high() {
                let irq = self.irq_status()?;
                self.clear_irq_status(IRQ_ALL)?;
                if irq & IRQ_CRC_ERR != 0 {
                    self.set_standby()?;
                    return Ok(None);
                }
                if irq & IRQ_RX_DONE != 0 {
                    let (length, start) = self.rx_buffer_status()?;
                    let n = (length as usize).min(buf.len());
                    self.read_buffer(start, &mut buf[..n])?;
                    (self.last_rssi_dbm, self.last_snr_db) = self.packet_status()?;
                    self.set_standby()?;
                    return Ok(Some(n));
                }
            }
            if elapsed >= timeout_ms {
                self.set_standby()?;
                return Ok(None);
            }
            self.delay.delay_millis(1);
            elapsed += 1;
        }
    }

    /// Put the radio into continuous receive mode without blocking, then call
    /// [`Lora::poll_receive`] repeatedly to collect packets as they arrive.
    pub fn start_receive(&mut self) -> Result<(), Error> {
        self.set_standby()?;
        self.set_packet_params(MAX_PAYLOAD as u8)?;
        self.clear_irq_status(IRQ_ALL)?;
        self.set_rx_continuous()
    }

    /// Non-blocking check for a received packet while in continuous receive
    /// mode (see [`Lora::start_receive`]).
    ///
    /// Returns `Ok(Some(n))` with the payload length when a packet has arrived
    /// since the last call, or `Ok(None)` if none is ready or one failed its
    /// CRC. The radio keeps listening either way, so this can be polled from a
    /// main loop. RSSI and SNR are then available via [`Lora::rssi`]/
    /// [`Lora::snr`].
    pub fn poll_receive(&mut self, buf: &mut [u8]) -> Result<Option<usize>, Error> {
        if self.dio1.is_low() {
            return Ok(None);
        }
        let irq = self.irq_status()?;
        self.clear_irq_status(IRQ_ALL)?;
        if irq & IRQ_RX_DONE == 0 || irq & IRQ_CRC_ERR != 0 {
            return Ok(None);
        }
        let (length, start) = self.rx_buffer_status()?;
        let n = (length as usize).min(buf.len());
        self.read_buffer(start, &mut buf[..n])?;
        (self.last_rssi_dbm, self.last_snr_db) = self.packet_status()?;
        Ok(Some(n))
    }

    /// Put the radio into a low-power standby state when receive mode is no
    /// longer needed. It returns to service via [`Lora::transmit`] or
    /// [`Lora::start_receive`].
    pub fn standby(&mut self) -> Result<(), Error> {
        self.set_standby()
    }

    /// RSSI of the most recently received packet, in dBm.
    pub fn rssi(&self) -> i16 {
        self.last_rssi_dbm
    }

    /// SNR of the most recently received packet, in dB.
    pub fn snr(&self) -> i16 {
        self.last_snr_db
    }

    /// Returns the radio's status byte (`GetStatus`). Bits 6:4 encode the chip
    /// mode and bits 3:1 the last command status.
    pub fn status(&mut self) -> Result<u8, Error> {
        let mut buf = [OP_GET_STATUS, 0x00];
        self.transfer(&mut buf)?;
        Ok(buf[1])
    }

    /// Returns the accumulated device error flags (`GetDeviceErrors`). Zero
    /// means no errors are latched.
    pub fn device_errors(&mut self) -> Result<u16, Error> {
        let mut buf = [OP_GET_DEVICE_ERRORS, 0x00, 0x00, 0x00];
        self.transfer(&mut buf)?;
        Ok(u16::from_be_bytes([buf[2], buf[3]]))
    }

    fn init(&mut self) -> Result<(), Error> {
        self.reset();
        self.set_standby()?;
        self.self_check()?;
        self.set_dio3_as_tcxo()?;
        self.delay.delay_millis(1);
        self.calibrate(CALIBRATE_ALL)?;
        self.delay.delay_millis(5);
        self.clear_device_errors()?;
        self.set_dio2_as_rf_switch()?;
        self.set_packet_type_lora()?;
        self.set_rf_frequency(self.config.frequency_hz)?;
        self.calibrate_image()?;
        self.set_pa_config()?;
        self.set_tx_params(self.config.tx_power_dbm)?;
        self.set_ocp()?;
        self.set_buffer_base_address()?;
        self.set_modulation_params()?;
        self.set_packet_params(0)?;
        self.set_sync_word(self.config.sync_word)?;
        self.set_dio_irq_params()
    }

    fn reset(&mut self) {
        self.rst.set_low();
        self.delay.delay_millis(2);
        self.rst.set_high();
        self.delay.delay_millis(5);
    }

    fn set_standby(&mut self) -> Result<(), Error> {
        self.write_command(&[OP_SET_STANDBY, STDBY_RC])
    }

    fn self_check(&mut self) -> Result<(), Error> {
        self.write_register(REG_LORA_SYNC_WORD, &SYNC_WORD_PRIVATE.to_be_bytes())?;
        let mut read_back = [0u8; 2];
        self.read_register(REG_LORA_SYNC_WORD, &mut read_back)?;
        let got = u16::from_be_bytes(read_back);
        if got != SYNC_WORD_PRIVATE {
            return Err(Error::CommsCheck {
                expected: SYNC_WORD_PRIVATE,
                got,
            });
        }
        Ok(())
    }

    fn set_dio3_as_tcxo(&mut self) -> Result<(), Error> {
        self.write_command(&[
            OP_SET_DIO3_AS_TCXO_CTRL,
            TCXO_VOLTAGE_2_4V,
            (TCXO_TIMEOUT >> 16) as u8,
            (TCXO_TIMEOUT >> 8) as u8,
            TCXO_TIMEOUT as u8,
        ])
    }

    fn calibrate(&mut self, blocks: u8) -> Result<(), Error> {
        self.write_command(&[OP_CALIBRATE, blocks])
    }

    fn calibrate_image(&mut self) -> Result<(), Error> {
        self.write_command(&[OP_CALIBRATE_IMAGE, IMG_CAL_902_928[0], IMG_CAL_902_928[1]])
    }

    fn clear_device_errors(&mut self) -> Result<(), Error> {
        self.write_command(&[OP_CLEAR_DEVICE_ERRORS, 0x00, 0x00])
    }

    fn set_dio2_as_rf_switch(&mut self) -> Result<(), Error> {
        self.write_command(&[OP_SET_DIO2_AS_RF_SWITCH, 0x01])
    }

    fn set_packet_type_lora(&mut self) -> Result<(), Error> {
        self.write_command(&[OP_SET_PACKET_TYPE, PACKET_TYPE_LORA])
    }

    fn set_rf_frequency(&mut self, hz: u32) -> Result<(), Error> {
        let reg = (((hz as u64) << 25) / XTAL_FREQUENCY_HZ) as u32;
        self.write_command(&[
            OP_SET_RF_FREQUENCY,
            (reg >> 24) as u8,
            (reg >> 16) as u8,
            (reg >> 8) as u8,
            reg as u8,
        ])
    }

    fn set_pa_config(&mut self) -> Result<(), Error> {
        // Optimal SX1262 PA settings for the +22 dBm-capable high-power path.
        self.write_command(&[OP_SET_PA_CONFIG, 0x04, 0x07, 0x00, 0x01])
    }

    fn set_tx_params(&mut self, power_dbm: i8) -> Result<(), Error> {
        let power = power_dbm.clamp(-9, 22) as u8;
        self.write_command(&[OP_SET_TX_PARAMS, power, RAMP_TIME_200US])
    }

    fn set_ocp(&mut self) -> Result<(), Error> {
        self.write_register(REG_OCP, &[OCP_140MA])
    }

    fn set_buffer_base_address(&mut self) -> Result<(), Error> {
        self.write_command(&[OP_SET_BUFFER_BASE_ADDRESS, 0x00, 0x00])
    }

    fn set_modulation_params(&mut self) -> Result<(), Error> {
        let sf = self.config.spreading_factor;
        let bw = self.config.bandwidth;
        self.write_command(&[
            OP_SET_MODULATION_PARAMS,
            sf.register(),
            bw.register(),
            self.config.coding_rate.register(),
            low_data_rate_optimize(sf, bw),
        ])
    }

    fn set_packet_params(&mut self, payload_len: u8) -> Result<(), Error> {
        let preamble = self.config.preamble_length;
        self.write_command(&[
            OP_SET_PACKET_PARAMS,
            (preamble >> 8) as u8,
            preamble as u8,
            HEADER_EXPLICIT,
            payload_len,
            CRC_ON,
            IQ_STANDARD,
        ])
    }

    fn set_sync_word(&mut self, sync_word: u16) -> Result<(), Error> {
        self.write_register(REG_LORA_SYNC_WORD, &sync_word.to_be_bytes())
    }

    fn set_dio_irq_params(&mut self) -> Result<(), Error> {
        let mask = IRQ_TX_DONE | IRQ_RX_DONE | IRQ_CRC_ERR | IRQ_TIMEOUT;
        self.write_command(&[
            OP_SET_DIO_IRQ_PARAMS,
            (mask >> 8) as u8,
            mask as u8,
            (mask >> 8) as u8,
            mask as u8,
            0x00,
            0x00,
            0x00,
            0x00,
        ])
    }

    fn set_tx(&mut self) -> Result<(), Error> {
        // Timeout 0 disables the radio's own timeout; we poll DIO1 instead.
        self.write_command(&[OP_SET_TX, 0x00, 0x00, 0x00])
    }

    fn write_buffer(&mut self, data: &[u8]) -> Result<(), Error> {
        let mut buf = [0u8; MAX_PAYLOAD + 2];
        buf[0] = OP_WRITE_BUFFER;
        buf[1] = 0x00;
        let end = 2 + data.len();
        buf[2..end].copy_from_slice(data);
        self.write_command(&buf[..end])
    }

    fn set_rx_continuous(&mut self) -> Result<(), Error> {
        // Timeout 0xFFFFFF selects continuous RX; we poll DIO1 with our own
        // software timeout instead.
        self.write_command(&[OP_SET_RX, 0xFF, 0xFF, 0xFF])
    }

    fn read_buffer(&mut self, offset: u8, out: &mut [u8]) -> Result<(), Error> {
        let mut buf = [0u8; MAX_PAYLOAD + 3];
        buf[0] = OP_READ_BUFFER;
        buf[1] = offset;
        let end = 3 + out.len();
        self.transfer(&mut buf[..end])?;
        out.copy_from_slice(&buf[3..end]);
        Ok(())
    }

    fn rx_buffer_status(&mut self) -> Result<(u8, u8), Error> {
        let mut buf = [OP_GET_RX_BUFFER_STATUS, 0x00, 0x00, 0x00];
        self.transfer(&mut buf)?;
        Ok((buf[2], buf[3]))
    }

    fn packet_status(&mut self) -> Result<(i16, i16), Error> {
        let mut buf = [OP_GET_PACKET_STATUS, 0x00, 0x00, 0x00, 0x00];
        self.transfer(&mut buf)?;
        let rssi = -(buf[2] as i16) / 2;
        let snr = (buf[3] as i8 as i16) / 4;
        Ok((rssi, snr))
    }

    fn irq_status(&mut self) -> Result<u16, Error> {
        let mut buf = [OP_GET_IRQ_STATUS, 0x00, 0x00, 0x00];
        self.transfer(&mut buf)?;
        Ok(u16::from_be_bytes([buf[2], buf[3]]))
    }

    fn clear_irq_status(&mut self, mask: u16) -> Result<(), Error> {
        self.write_command(&[OP_CLEAR_IRQ_STATUS, (mask >> 8) as u8, mask as u8])
    }

    fn wait_tx_done(&mut self) -> Result<(), Error> {
        let mut iters = 0;
        loop {
            if self.dio1.is_high() {
                let irq = self.irq_status()?;
                if irq & IRQ_TIMEOUT != 0 {
                    return Err(Error::TxTimeout);
                }
                if irq & IRQ_TX_DONE != 0 {
                    return Ok(());
                }
            }
            if iters >= TX_TIMEOUT_ITERS {
                return Err(Error::TxTimeout);
            }
            self.delay.delay_millis(1);
            iters += 1;
        }
    }

    fn write_register(&mut self, addr: u16, data: &[u8]) -> Result<(), Error> {
        let mut buf = [0u8; 8];
        buf[0] = OP_WRITE_REGISTER;
        buf[1] = (addr >> 8) as u8;
        buf[2] = addr as u8;
        let end = 3 + data.len();
        buf[3..end].copy_from_slice(data);
        self.write_command(&buf[..end])
    }

    fn read_register(&mut self, addr: u16, out: &mut [u8]) -> Result<(), Error> {
        let mut buf = [0u8; 8];
        buf[0] = OP_READ_REGISTER;
        buf[1] = (addr >> 8) as u8;
        buf[2] = addr as u8;
        let end = 4 + out.len();
        self.transfer(&mut buf[..end])?;
        out.copy_from_slice(&buf[4..end]);
        Ok(())
    }

    fn write_command(&mut self, buf: &[u8]) -> Result<(), Error> {
        self.wait_busy()?;
        // the SpiDevice asserts/deasserts CS and applies the lora clock/mode for
        // the duration; wait_busy stays before it, since CS must not go low until
        // the radio is ready.
        self.device.write(buf).map_err(bus_err)
    }

    fn transfer(&mut self, buf: &mut [u8]) -> Result<(), Error> {
        self.wait_busy()?;
        self.device.transfer_in_place(buf).map_err(bus_err)
    }

    fn wait_busy(&mut self) -> Result<(), Error> {
        let mut iters = 0;
        while self.busy.is_high() {
            if iters >= BUSY_TIMEOUT_ITERS {
                return Err(Error::BusyTimeout);
            }
            self.delay.delay_micros(BUSY_POLL_INTERVAL_US);
            iters += 1;
        }
        Ok(())
    }
}

// fold the shared-bus device error into the driver's error type.
fn bus_err(e: crate::bus::Error) -> Error {
    match e {
        crate::bus::Error::Spi(e) => Error::Spi(e),
        crate::bus::Error::Config(e) => Error::SpiConfig(e),
    }
}

fn low_data_rate_optimize(sf: SpreadingFactor, bw: Bandwidth) -> u8 {
    let symbol_us = ((1u64 << sf.register()) * 1_000_000) / bw.hz() as u64;
    u8::from(symbol_us >= 16_380)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Error {
    SpiConfig(SpiConfigError),
    Spi(esp_hal::spi::Error),
    /// The BUSY line never went low within the timeout.
    BusyTimeout,
    /// The radio did not report TxDone within the timeout.
    TxTimeout,
    /// The payload exceeds the radio's maximum packet length.
    PayloadTooLong(usize),
    /// A register written during the self-check did not read back correctly,
    /// indicating the SPI link or BUSY handshake is not working.
    CommsCheck {
        expected: u16,
        got: u16,
    },
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::SpiConfig(e) => write!(f, "lora spi configuration failed: {e:?}"),
            Self::Spi(e) => write!(f, "lora spi transfer error: {e:?}"),
            Self::BusyTimeout => write!(f, "timed out waiting for lora BUSY to clear"),
            Self::TxTimeout => write!(f, "timed out waiting for lora transmit to complete"),
            Self::PayloadTooLong(len) => {
                write!(f, "lora payload of {len} bytes exceeds {MAX_PAYLOAD}")
            }
            Self::CommsCheck { expected, got } => write!(
                f,
                "lora comms check failed: wrote {expected:#06x}, read back {got:#06x}"
            ),
        }
    }
}
