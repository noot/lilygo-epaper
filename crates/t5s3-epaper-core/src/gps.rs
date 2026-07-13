use esp_hal::{
    delay::Delay,
    peripherals,
    uart::{self, Uart},
    Blocking,
};
use log::debug;

const LINE_BUF_SIZE: usize = 128;
const L76K_BAUD: u32 = 9600;
const MIA_M10Q_BAUD: u32 = 38400;
const DETECT_TIMEOUT_MS: u32 = 2000;

/// GPS module variant present on the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Module {
    /// Quectel L76K — 9600 baud default, PCAS command set.
    L76K,
    /// u-blox MIA-M10Q — 38400 baud default, UBX/NMEA.
    MiaM10Q,
}

impl Module {
    fn baud_rate(self) -> u32 {
        match self {
            Self::L76K => L76K_BAUD,
            Self::MiaM10Q => MIA_M10Q_BAUD,
        }
    }
}

pub struct PinConfig<'a> {
    pub tx: peripherals::GPIO43<'a>,
    pub rx: peripherals::GPIO44<'a>,
}

/// GPS receiver driver.
///
/// Wraps a UART connection to either an L76K or MIA-M10Q GPS module and
/// provides parsed NMEA data. The GPS/LoRa 3.3 V power rail (PCA9535 IO0_0)
/// is enabled by `Display::new()`, so the module is powered for the rest of
/// this power cycle, until `Display::deep_sleep()` cuts the rail.
///
/// Call [`Gps::update()`] in your main loop to read new NMEA data from
/// the UART and update the internal parser state.
pub struct Gps<'a> {
    uart: Uart<'a, Blocking>,
    parser: nmea::Nmea,
    line_buf: [u8; LINE_BUF_SIZE],
    line_pos: usize,
    module: Module,
}

impl<'a> Gps<'a> {
    /// Create a new GPS driver on the given UART peripheral.
    ///
    /// Configures the UART at the correct baud rate for the specified module
    /// and, for the L76K, sends initial configuration commands to enable all
    /// NMEA sentences and GPS + GLONASS.
    pub fn new(
        uart_peripheral: peripherals::UART1<'a>,
        pins: PinConfig<'a>,
        module: Module,
        delay: &mut Delay,
    ) -> Result<Self, Error> {
        let config = uart::Config::default().with_baudrate(module.baud_rate());
        let uart = Uart::new(uart_peripheral, config)
            .map_err(Error::UartConfig)?
            .with_rx(pins.rx)
            .with_tx(pins.tx);

        let mut gps = Self {
            uart,
            parser: nmea::Nmea::default(),
            line_buf: [0u8; LINE_BUF_SIZE],
            line_pos: 0,
            module,
        };

        if module == Module::L76K {
            gps.configure_l76k(delay)?;
        }

        debug!(
            "gps: initialized {:?} at {} baud",
            module,
            module.baud_rate()
        );
        Ok(gps)
    }

    /// Detect the GPS module type automatically and create a driver.
    ///
    /// Tries L76K at 9600 baud first (sends a version query and looks for a
    /// `$GPTXT` response), then falls back to MIA-M10Q at 38400 baud by
    /// listening for any NMEA output.
    pub fn detect(
        uart_peripheral: peripherals::UART1<'a>,
        pins: PinConfig<'a>,
        delay: &mut Delay,
    ) -> Result<Self, Error> {
        let config = uart::Config::default().with_baudrate(L76K_BAUD);
        let uart = Uart::new(uart_peripheral, config)
            .map_err(Error::UartConfig)?
            .with_rx(pins.rx)
            .with_tx(pins.tx);

        let mut gps = Self {
            uart,
            parser: nmea::Nmea::default(),
            line_buf: [0u8; LINE_BUF_SIZE],
            line_pos: 0,
            module: Module::L76K,
        };

        if gps.probe_l76k(delay)? {
            debug!("gps: detected L76K at 9600 baud");
            gps.configure_l76k(delay)?;
            return Ok(gps);
        }

        debug!("gps: L76K not found, trying MIA-M10Q at 38400 baud");
        gps.uart
            .apply_config(&uart::Config::default().with_baudrate(MIA_M10Q_BAUD))
            .map_err(Error::UartConfig)?;
        gps.module = Module::MiaM10Q;

        if gps.probe_nmea(delay)? {
            debug!("gps: detected MIA-M10Q at 38400 baud");
            return Ok(gps);
        }

        Err(Error::NoModuleDetected)
    }

    /// Returns which GPS module this driver is configured for.
    pub fn module(&self) -> Module {
        self.module
    }

    /// Read available bytes from the UART and parse any complete NMEA
    /// sentences. Call this repeatedly from your main loop.
    ///
    /// Returns the number of sentences successfully parsed in this call.
    pub fn update(&mut self) -> Result<usize, Error> {
        let mut read_buf = [0u8; 128];
        let mut parsed = 0;

        loop {
            let n = match self.uart.read_buffered(&mut read_buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(esp_hal::uart::RxError::FifoOverflowed) => {
                    self.line_pos = 0;
                    continue;
                }
                Err(e) => return Err(Error::UartRead(e)),
            };

            for &byte in &read_buf[..n] {
                if byte == b'\n' {
                    if self.line_pos > 0 {
                        let end = if self.line_buf[self.line_pos - 1] == b'\r' {
                            self.line_pos - 1
                        } else {
                            self.line_pos
                        };

                        if let Ok(sentence) = core::str::from_utf8(&self.line_buf[..end]) {
                            debug!("gps: nmea: {sentence}");
                            match self.parser.parse(sentence) {
                                Ok(_) => parsed += 1,
                                Err(e) => debug!("gps: parse error: {:?}", e),
                            }
                        }
                    }
                    self.line_pos = 0;
                } else if self.line_pos < LINE_BUF_SIZE {
                    self.line_buf[self.line_pos] = byte;
                    self.line_pos += 1;
                } else {
                    // line too long, discard
                    self.line_pos = 0;
                }
            }
        }

        Ok(parsed)
    }

    /// Returns the last known latitude and longitude in degrees, if a fix has
    /// been obtained.
    pub fn location(&self) -> Option<(f64, f64)> {
        match (self.parser.latitude, self.parser.longitude) {
            (Some(lat), Some(lng)) => Some((lat, lng)),
            _ => None,
        }
    }

    /// Returns the altitude in meters above the WGS-84 ellipsoid, if
    /// available.
    pub fn altitude(&self) -> Option<f32> {
        self.parser.altitude
    }

    /// Returns the speed over ground in knots, if available.
    pub fn speed_over_ground(&self) -> Option<f32> {
        self.parser.speed_over_ground
    }

    /// Returns the true course over ground in degrees, if available.
    pub fn course(&self) -> Option<f32> {
        self.parser.true_course
    }

    /// Returns the number of satellites used in the current fix.
    pub fn fix_satellites(&self) -> Option<u32> {
        self.parser.num_of_fix_satellites
    }

    /// Returns the number of satellites currently in view, as reported by GSV
    /// sentences. Non-zero before a fix is acquired, so it is useful as a
    /// "the receiver is tracking" indicator.
    pub fn satellites_in_view(&self) -> usize {
        self.parser.satellites().len()
    }

    /// Returns the current fix type (None, GPS, DGPS, etc).
    pub fn fix_type(&self) -> Option<nmea::sentences::FixType> {
        self.parser.fix_type
    }

    /// Returns UTC as whole seconds since the Unix epoch, once RMC sentences
    /// have supplied both time and date. NMEA fractional seconds are
    /// truncated.
    pub fn utc_seconds(&self) -> Option<u64> {
        let time = self.parser.fix_time?;
        let date = self.parser.fix_date?;
        u64::try_from(date.and_time(time).and_utc().timestamp()).ok()
    }

    /// Returns the horizontal dilution of precision, if available.
    pub fn hdop(&self) -> Option<f32> {
        self.parser.hdop
    }

    /// Returns the vertical dilution of precision, if available. Populated by
    /// GSA sentences; a high value indicates a poor altitude solution.
    pub fn vdop(&self) -> Option<f32> {
        self.parser.vdop
    }

    fn probe_l76k(&mut self, delay: &mut Delay) -> Result<bool, Error> {
        // drain any stale data
        self.drain_uart();

        // send L76K version query
        self.send_command(b"$PCAS06,0*1B\r\n", delay)?;

        // look for $GPTXT response within the timeout
        let mut elapsed = 0u32;
        while elapsed < DETECT_TIMEOUT_MS {
            let mut buf = [0u8; 64];
            if let Ok(n) = self.uart.read_buffered(&mut buf) {
                for &byte in &buf[..n] {
                    if byte == b'\n' {
                        if self.line_pos > 0 {
                            if let Ok(line) = core::str::from_utf8(&self.line_buf[..self.line_pos])
                            {
                                if line.contains("$GPTXT,01,01,02") {
                                    debug!("gps: L76K responded: {}", line);
                                    self.line_pos = 0;
                                    return Ok(true);
                                }
                            }
                        }
                        self.line_pos = 0;
                    } else if self.line_pos < LINE_BUF_SIZE {
                        self.line_buf[self.line_pos] = byte;
                        self.line_pos += 1;
                    } else {
                        self.line_pos = 0;
                    }
                }
            }
            delay.delay_millis(50);
            elapsed += 50;
        }

        self.line_pos = 0;
        Ok(false)
    }

    fn probe_nmea(&mut self, delay: &mut Delay) -> Result<bool, Error> {
        self.drain_uart();

        // listen for any data that starts with '$'
        let mut elapsed = 0u32;
        while elapsed < DETECT_TIMEOUT_MS {
            let mut buf = [0u8; 64];
            if let Ok(n) = self.uart.read_buffered(&mut buf) {
                if buf[..n].contains(&b'$') {
                    debug!("gps: received NMEA data at current baud rate");
                    return Ok(true);
                }
            }
            delay.delay_millis(50);
            elapsed += 50;
        }

        Ok(false)
    }

    fn drain_uart(&mut self) {
        let mut buf = [0u8; 128];
        while self.uart.read_buffered(&mut buf).unwrap_or(0) > 0 {}
    }

    fn configure_l76k(&mut self, delay: &mut Delay) -> Result<(), Error> {
        // disable all NMEA output while reconfiguring
        self.send_command(b"$PCAS03,0,0,0,0,0,0,0,0,0,0,,,0,0*02\r\n", delay)?;
        // enable GPS + GLONASS
        self.send_command(b"$PCAS04,5*1C\r\n", delay)?;
        // enable GGA + GSA + GSV + RMC: GGA/GSA/RMC carry the fix and position
        // this driver exposes, and GSV adds satellites-in-view so callers can
        // show tracking progress before a fix locks. (the 4th field is nGSV.)
        self.send_command(b"$PCAS03,1,0,1,1,1,0,0,0,0,0,,,0,0*02\r\n", delay)?;
        // vehicle navigation mode
        self.send_command(b"$PCAS11,3*1E\r\n", delay)?;
        Ok(())
    }

    fn send_command(&mut self, cmd: &[u8], delay: &mut Delay) -> Result<(), Error> {
        self.uart.write(cmd).map_err(Error::UartWrite)?;
        self.uart.flush().map_err(Error::UartWrite)?;
        delay.delay_millis(50);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Error {
    UartConfig(uart::ConfigError),
    UartRead(uart::RxError),
    UartWrite(uart::TxError),
    /// Neither L76K nor MIA-M10Q responded during auto-detection.
    NoModuleDetected,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UartConfig(e) => write!(f, "gps uart configuration failed: {e:?}"),
            Self::UartRead(e) => write!(f, "gps uart read error: {e}"),
            Self::UartWrite(e) => write!(f, "gps uart write error: {e}"),
            Self::NoModuleDetected => write!(f, "no GPS module detected"),
        }
    }
}
