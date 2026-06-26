//! Blocking driver for the Semtech SX1262 LoRa transceiver.
//!
//! The driver talks to the radio over an `embedded-hal` SPI bus plus four GPIOs:
//! NSS/CS and RESET (outputs), BUSY and DIO1 (inputs). It drives the chip-select
//! line itself and uses single full-duplex transfers, which is what the SX126x
//! expects. Transmit/receive completion is detected by polling the DIO1 line
//! (the radio holds BUSY high during a transmit, so SPI cannot be used to read
//! the IRQ status until DIO1 signals done). It is configured for LoRa
//! point-to-point transmit and receive.

mod command;
mod error;
pub mod params;

use embedded_hal::delay::DelayNs;
use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal::spi::SpiBus;

use command as cmd;
pub use error::Error;
pub use params::{Bandwidth, CodingRate, Config, SpreadingFactor};

/// the busy line should drop within ~1 ms; poll for up to ~100 ms.
const BUSY_POLL_ITERS: u32 = 1_000;
const BUSY_POLL_US: u32 = 100;

/// upper bound on how long a transmit may take before we give up (~5 s).
const TX_POLL_ITERS: u32 = 5_000;
const IRQ_POLL_MS: u32 = 1;

/// Details of a received packet.
#[derive(Debug, Clone, Copy)]
pub struct RxInfo {
    /// Number of payload bytes written into the caller's buffer.
    pub len: usize,
    /// Received signal strength in dBm.
    pub rssi_dbm: i16,
    /// Signal-to-noise ratio in dB.
    pub snr_db: i16,
}

/// Blocking SX1262 LoRa driver.
pub struct Sx1262<SPI, CS, RST, BUSY, DIO1, DELAY> {
    spi: SPI,
    cs: CS,
    rst: RST,
    busy: BUSY,
    dio1: DIO1,
    delay: DELAY,
    config: Config,
}

impl<SPI, CS, RST, BUSY, DIO1, DELAY, PE> Sx1262<SPI, CS, RST, BUSY, DIO1, DELAY>
where
    SPI: SpiBus<u8>,
    CS: OutputPin<Error = PE>,
    RST: OutputPin<Error = PE>,
    BUSY: InputPin<Error = PE>,
    DIO1: InputPin<Error = PE>,
    DELAY: DelayNs,
{
    /// Create a driver from the SPI bus, control pins and a delay source.
    pub fn new(
        spi: SPI,
        cs: CS,
        rst: RST,
        busy: BUSY,
        dio1: DIO1,
        delay: DELAY,
        config: Config,
    ) -> Self {
        Self {
            spi,
            cs,
            rst,
            busy,
            dio1,
            delay,
            config,
        }
    }

    /// Reset and configure the radio for LoRa operation using the [`Config`]
    /// supplied to [`new`](Self::new).
    pub fn init(&mut self) -> Result<(), Error<SPI::Error, PE>> {
        // this follows RadioLib's proven order: reset -> setTCXO -> config (buffer
        // base, packet type, fallback, clear irq, calibrate) -> regulator DC-DC ->
        // public settings (freq, modulation, sync, power, image cal, rf switch, irq).
        self.reset()?;
        self.write_cmd(cmd::SET_STANDBY, &[cmd::STDBY_RC])?;
        self.write_cmd(cmd::CLEAR_DEVICE_ERRORS, &[0x00, 0x00])?;

        // this board has a tcxo (no crystal) powered from dio3.
        self.write_cmd(
            cmd::SET_DIO3_AS_TCXO,
            &[
                cmd::TCXO_VOLTAGE_1_6,
                cmd::TCXO_TIMEOUT[0],
                cmd::TCXO_TIMEOUT[1],
                cmd::TCXO_TIMEOUT[2],
            ],
        )?;

        // config(): buffer base, packet type, fallback, clear irqs, then calibrate
        // (the xosc-dependent calibration runs after the tcxo is armed).
        self.write_cmd(cmd::SET_BUFFER_BASE_ADDRESS, &[0x00, 0x00])?;
        self.write_cmd(cmd::SET_PACKET_TYPE, &[cmd::PACKET_TYPE_LORA])?;
        self.write_cmd(cmd::SET_RX_TX_FALLBACK_MODE, &[cmd::FALLBACK_STDBY_RC])?;
        self.clear_irq_status(cmd::IRQ_ALL)?;
        self.write_cmd(cmd::CALIBRATE, &[cmd::CALIBRATE_ALL])?;
        self.delay.delay_ms(5);
        self.wait_busy()?;

        // switch to the dc-dc regulator AFTER calibration (RadioLib order).
        self.write_cmd(cmd::SET_REGULATOR_MODE, &[cmd::REGULATOR_DC_DC])?;

        // public settings.
        let pll = self.config.frequency_pll().to_be_bytes();
        self.write_cmd(cmd::SET_RF_FREQUENCY, &pll)?;
        self.set_modulation_params()?;
        self.set_packet_params(0)?;

        // set the lora sync word (now that we're in LoRa packet type).
        let sync = self.config.sync_word.to_be_bytes();
        self.write_register(cmd::REG_LORA_SYNC_WORD, &sync)?;

        self.write_cmd(cmd::SET_PA_CONFIG, &cmd::PA_CONFIG_SX1262)?;
        let power = self.config.tx_power_dbm.clamp(-9, 22) as u8;
        self.write_cmd(cmd::SET_TX_PARAMS, &[power, cmd::RAMP_200_US])?;
        self.write_register(cmd::REG_OCP, &[cmd::OCP_140_MA])?;

        let band = self.config.calibrate_image_band();
        self.write_cmd(cmd::CALIBRATE_IMAGE, &band)?;

        // dio2 drives the antenna rf switch; route the irqs to dio1.
        self.write_cmd(cmd::SET_DIO2_AS_RF_SWITCH, &[0x01])?;
        self.set_dio_irq_params()?;
        Ok(())
    }

    /// Transmit a LoRa packet, blocking until the radio reports TxDone.
    pub fn transmit(&mut self, data: &[u8]) -> Result<(), Error<SPI::Error, PE>> {
        if data.is_empty() || data.len() > 255 {
            return Err(Error::BufferTooSmall);
        }
        self.write_cmd(cmd::SET_STANDBY, &[cmd::STDBY_RC])?;
        self.set_packet_params(data.len() as u8)?;
        self.write_buffer(0, data)?;
        self.clear_irq_status(cmd::IRQ_ALL)?;

        // set_tx with a zero timeout means transmit until done. busy stays high
        // during transmission, so wait on the dio1 line, then read the irq.
        self.write_cmd(cmd::SET_TX, &[0x00, 0x00, 0x00])?;
        self.wait_dio1(Some(TX_POLL_ITERS))?;
        let irq = self.get_irq_status()?;
        self.clear_irq_status(cmd::IRQ_ALL)?;
        if irq & cmd::IRQ_TX_DONE == 0 {
            return Err(Error::Timeout);
        }
        Ok(())
    }

    /// Receive a single LoRa packet, blocking until one arrives.
    ///
    /// The payload is written into `buf` and the returned [`RxInfo`] reports its
    /// length along with signal quality.
    pub fn receive(&mut self, buf: &mut [u8]) -> Result<RxInfo, Error<SPI::Error, PE>> {
        self.write_cmd(cmd::SET_STANDBY, &[cmd::STDBY_RC])?;
        self.set_packet_params(0xFF)?;
        self.clear_irq_status(cmd::IRQ_ALL)?;

        // set_rx with 0xFFFFFF is continuous receive; block until dio1 signals.
        self.write_cmd(cmd::SET_RX, &[0xFF, 0xFF, 0xFF])?;
        self.wait_dio1(None)?;
        let irq = self.get_irq_status()?;
        self.clear_irq_status(cmd::IRQ_ALL)?;
        if irq & cmd::IRQ_CRC_ERR != 0 {
            return Err(Error::CrcMismatch);
        }
        if irq & cmd::IRQ_RX_DONE == 0 {
            return Err(Error::Timeout);
        }

        let (len, start) = self.get_rx_buffer_status()?;
        let len = len as usize;
        if len > buf.len() {
            return Err(Error::BufferTooSmall);
        }
        self.read_buffer(start, &mut buf[..len])?;
        let (rssi_dbm, snr_db) = self.get_packet_status()?;
        Ok(RxInfo {
            len,
            rssi_dbm,
            snr_db,
        })
    }

    /// Read the radio's status byte (`GetStatus`). Bits 6:4 encode the chip mode
    /// and bits 3:1 the status of the last command.
    pub fn status(&mut self) -> Result<u8, Error<SPI::Error, PE>> {
        let mut resp = [0u8; 1];
        self.read_cmd(cmd::GET_STATUS, &mut resp)?;
        Ok(resp[0])
    }

    /// Read the latched device-error flags (`GetDeviceErrors`). Zero means no
    /// errors; e.g. bit 5 (`0x20`) is `XOSC_START_ERR` (TCXO/clock failure).
    pub fn device_errors(&mut self) -> Result<u16, Error<SPI::Error, PE>> {
        let mut resp = [0u8; 3];
        self.read_cmd(cmd::GET_DEVICE_ERRORS, &mut resp)?;
        Ok(u16::from_be_bytes([resp[1], resp[2]]))
    }

    // -- bring-up helpers ---------------------------------------------------

    fn reset(&mut self) -> Result<(), Error<SPI::Error, PE>> {
        self.cs.set_high().map_err(Error::Pin)?;
        self.rst.set_low().map_err(Error::Pin)?;
        self.delay.delay_ms(2);
        self.rst.set_high().map_err(Error::Pin)?;
        self.delay.delay_ms(5);
        self.wait_busy()
    }

    fn set_modulation_params(&mut self) -> Result<(), Error<SPI::Error, PE>> {
        self.write_cmd(
            cmd::SET_MODULATION_PARAMS,
            &[
                self.config.spreading_factor.reg(),
                self.config.bandwidth.reg(),
                self.config.coding_rate.reg(),
                self.config.low_data_rate_optimize(),
            ],
        )
    }

    fn set_packet_params(&mut self, payload_len: u8) -> Result<(), Error<SPI::Error, PE>> {
        let preamble = self.config.preamble_len.to_be_bytes();
        self.write_cmd(
            cmd::SET_PACKET_PARAMS,
            &[
                preamble[0],
                preamble[1],
                cmd::HEADER_EXPLICIT,
                payload_len,
                cmd::CRC_ON,
                cmd::IQ_STANDARD,
            ],
        )
    }

    fn set_dio_irq_params(&mut self) -> Result<(), Error<SPI::Error, PE>> {
        let mask = cmd::IRQ_TX_DONE | cmd::IRQ_RX_DONE | cmd::IRQ_CRC_ERR | cmd::IRQ_TIMEOUT;
        let m = mask.to_be_bytes();
        // enable `mask` globally and route it to dio1; dio2/dio3 unused.
        self.write_cmd(
            cmd::SET_DIO_IRQ_PARAMS,
            &[m[0], m[1], m[0], m[1], 0x00, 0x00, 0x00, 0x00],
        )
    }

    // -- status -------------------------------------------------------------

    fn get_irq_status(&mut self) -> Result<u16, Error<SPI::Error, PE>> {
        let mut resp = [0u8; 3];
        self.read_cmd(cmd::GET_IRQ_STATUS, &mut resp)?;
        Ok(u16::from_be_bytes([resp[1], resp[2]]))
    }

    fn clear_irq_status(&mut self, mask: u16) -> Result<(), Error<SPI::Error, PE>> {
        let m = mask.to_be_bytes();
        self.write_cmd(cmd::CLEAR_IRQ_STATUS, &m)
    }

    fn get_rx_buffer_status(&mut self) -> Result<(u8, u8), Error<SPI::Error, PE>> {
        let mut resp = [0u8; 3];
        self.read_cmd(cmd::GET_RX_BUFFER_STATUS, &mut resp)?;
        // resp = [status, payload_len, rx_start_buffer_pointer]
        Ok((resp[1], resp[2]))
    }

    fn get_packet_status(&mut self) -> Result<(i16, i16), Error<SPI::Error, PE>> {
        let mut resp = [0u8; 4];
        self.read_cmd(cmd::GET_PACKET_STATUS, &mut resp)?;
        // resp = [status, rssi_pkt, snr_pkt, signal_rssi_pkt]
        let rssi = -(resp[1] as i16) / 2;
        let snr = (resp[2] as i8 as i16) / 4;
        Ok((rssi, snr))
    }

    // -- low-level spi ------------------------------------------------------
    //
    // every exchange is a single full-duplex transfer bracketed by manual cs;
    // split write-then-read transactions do not read back reliably on esp-hal.

    fn write_cmd(&mut self, opcode: u8, params: &[u8]) -> Result<(), Error<SPI::Error, PE>> {
        self.wait_busy()?;
        let mut buf = [0u8; 16];
        buf[0] = opcode;
        let end = 1 + params.len();
        buf[1..end].copy_from_slice(params);
        self.transfer(&mut buf[..end])?;
        Ok(())
    }

    fn read_cmd(&mut self, opcode: u8, resp: &mut [u8]) -> Result<(), Error<SPI::Error, PE>> {
        self.wait_busy()?;
        // byte 0 clocks out the opcode (response is the chip status, discarded);
        // the response bytes follow.
        let mut buf = [0u8; 8];
        buf[0] = opcode;
        let end = 1 + resp.len();
        self.transfer(&mut buf[..end])?;
        resp.copy_from_slice(&buf[1..end]);
        Ok(())
    }

    fn write_register(&mut self, addr: u16, data: &[u8]) -> Result<(), Error<SPI::Error, PE>> {
        self.wait_busy()?;
        let mut buf = [0u8; 16];
        buf[0] = cmd::WRITE_REGISTER;
        buf[1] = (addr >> 8) as u8;
        buf[2] = addr as u8;
        let end = 3 + data.len();
        buf[3..end].copy_from_slice(data);
        self.transfer(&mut buf[..end])?;
        Ok(())
    }

    fn write_buffer(&mut self, offset: u8, data: &[u8]) -> Result<(), Error<SPI::Error, PE>> {
        self.wait_busy()?;
        let mut buf = [0u8; 258];
        buf[0] = cmd::WRITE_BUFFER;
        buf[1] = offset;
        let end = 2 + data.len();
        buf[2..end].copy_from_slice(data);
        self.transfer(&mut buf[..end])?;
        Ok(())
    }

    fn read_buffer(&mut self, offset: u8, data: &mut [u8]) -> Result<(), Error<SPI::Error, PE>> {
        self.wait_busy()?;
        // opcode + offset + one status byte precede the payload.
        let mut buf = [0u8; 259];
        buf[0] = cmd::READ_BUFFER;
        buf[1] = offset;
        let end = 3 + data.len();
        self.transfer(&mut buf[..end])?;
        data.copy_from_slice(&buf[3..end]);
        Ok(())
    }

    /// Full-duplex transfer with manual chip-select bracketing.
    fn transfer(&mut self, buf: &mut [u8]) -> Result<(), Error<SPI::Error, PE>> {
        self.cs.set_low().map_err(Error::Pin)?;
        let result = self.spi.transfer_in_place(buf).map_err(Error::Spi);
        self.cs.set_high().map_err(Error::Pin)?;
        result
    }

    // -- polling ------------------------------------------------------------

    fn wait_busy(&mut self) -> Result<(), Error<SPI::Error, PE>> {
        for _ in 0..BUSY_POLL_ITERS {
            if self.busy.is_low().map_err(Error::Pin)? {
                return Ok(());
            }
            self.delay.delay_us(BUSY_POLL_US);
        }
        Err(Error::Timeout)
    }

    /// Block until the DIO1 line goes high (an enabled IRQ fired). The pin is
    /// polled rather than the IRQ register because BUSY is held high during a
    /// transmit, which forbids SPI access. With `Some(iters)` give up after that
    /// many polls; with `None` wait indefinitely (used for continuous receive).
    fn wait_dio1(&mut self, max_iters: Option<u32>) -> Result<(), Error<SPI::Error, PE>> {
        let mut count = 0u32;
        loop {
            if self.dio1.is_high().map_err(Error::Pin)? {
                return Ok(());
            }
            if let Some(limit) = max_iters {
                count += 1;
                if count >= limit {
                    return Err(Error::Timeout);
                }
            }
            self.delay.delay_ms(IRQ_POLL_MS);
        }
    }
}
