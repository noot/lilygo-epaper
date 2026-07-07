use core::cell::RefCell;

use embedded_hal::{
    delay::DelayNs as _,
    spi::{Error as SpiError, ErrorKind, ErrorType, Operation, SpiBus, SpiDevice},
};
use esp_hal::{
    delay::Delay,
    gpio::{Level, Output, OutputConfig},
    peripherals,
    spi::{
        master::{Config as SpiConfig, ConfigError, Spi},
        Mode as SpiMode,
    },
    Blocking,
};

/// The shared SPI2 bus plus every chip-select on it.
///
/// The bus (SPI2 and the sclk/mosi/miso lines) is owned once, and so are both
/// devices' chip-selects, parked high except while their device is
/// mid-transaction. "Device not in use" is therefore a first-class state: an
/// idle chip can never see a floating select and respond to another device's
/// traffic. (A floating LoRa select used to make SD-card init intermittently
/// fail with `CardNotFound` — the idle SX1262 held MISO — and a floating SD
/// select would let radio traffic clock noise into the card.)
pub struct Bus<'d> {
    pub(crate) spi: RefCell<Spi<'d, Blocking>>,
    pub(crate) sd_cs: RefCell<Output<'d>>,
    pub(crate) lora_cs: RefCell<Output<'d>>,
}

impl<'d> Bus<'d> {
    /// Build the shared bus, taking ownership of the SD-card and LoRa
    /// chip-selects and parking them high. The returned bus must outlive any
    /// device created from it.
    pub fn new(
        spi: peripherals::SPI2<'d>,
        sclk: peripherals::GPIO14<'d>,
        mosi: peripherals::GPIO13<'d>,
        miso: peripherals::GPIO21<'d>,
        sd_cs: peripherals::GPIO12<'d>,
        lora_cs: peripherals::GPIO46<'d>,
    ) -> core::result::Result<Self, ConfigError> {
        let spi = Spi::new(spi, SpiConfig::default().with_mode(SpiMode::_0))?
            .with_sck(sclk)
            .with_mosi(mosi)
            .with_miso(miso);
        Ok(Self {
            spi: RefCell::new(spi),
            sd_cs: RefCell::new(Output::new(sd_cs, Level::High, OutputConfig::default())),
            lora_cs: RefCell::new(Output::new(lora_cs, Level::High, OutputConfig::default())),
        })
    }
}

// one device on the shared SPI bus. the bus and the device's chip-select are
// both owned by [`Bus`] and borrowed here by reference, carrying the device's
// clock/mode. borrowing the bus through the RefCell is what serialises access:
// a second concurrent user panics instead of silently corrupting the bus. the
// chip-select is only ever toggled inside a transaction, so it parks high (in
// the Bus) whenever the device is idle or dropped.
pub(crate) struct SharedSpiDevice<'a, 'd> {
    bus: &'a RefCell<Spi<'d, Blocking>>,
    cs: &'a RefCell<Output<'d>>,
    config: SpiConfig,
}

impl<'a, 'd> SharedSpiDevice<'a, 'd> {
    pub(crate) fn new(
        bus: &'a RefCell<Spi<'d, Blocking>>,
        cs: &'a RefCell<Output<'d>>,
        config: SpiConfig,
    ) -> Self {
        Self { bus, cs, config }
    }

    // change the clock/mode applied on subsequent transactions, e.g. to raise
    // the sd clock after the <=400 kHz acquisition phase.
    pub(crate) fn set_config(&mut self, config: SpiConfig) {
        self.config = config;
    }
}

// the SpiDevice error must fold in both the bus transfer error and the
// per-device apply_config error, since transaction() does both.
#[derive(Debug)]
pub(crate) enum Error {
    Spi(esp_hal::spi::Error),
    Config(ConfigError),
}

impl SpiError for Error {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::Spi(e) => e.kind(),
            Self::Config(_) => ErrorKind::Other,
        }
    }
}

impl ErrorType for SharedSpiDevice<'_, '_> {
    type Error = Error;
}

impl SpiDevice for SharedSpiDevice<'_, '_> {
    fn transaction(&mut self, ops: &mut [Operation<'_, u8>]) -> Result<(), Self::Error> {
        // borrowing the shared bus here is the exclusivity guarantee.
        let mut bus = self.bus.borrow_mut();
        // the previous user may have left the bus at a different clock/mode.
        bus.apply_config(&self.config).map_err(Error::Config)?;

        let mut cs = self.cs.borrow_mut();
        cs.set_low();
        let op_res = ops.iter_mut().try_for_each(|op| match op {
            Operation::Read(buf) => bus.read(buf),
            Operation::Write(buf) => bus.write(buf),
            // fully qualified: Spi has an inherent one-buffer `transfer` that
            // would otherwise shadow SpiBus's two-buffer method.
            Operation::Transfer(read, write) => SpiBus::transfer(&mut *bus, read, write),
            Operation::TransferInPlace(buf) => bus.transfer_in_place(buf),
            Operation::DelayNs(ns) => {
                bus.flush()?;
                Delay::new().delay_ns(*ns);
                Ok(())
            }
        });
        // flush before releasing chip-select so the last word is clocked out.
        let flush_res = bus.flush();
        cs.set_high();

        op_res.and(flush_res).map_err(Error::Spi)
    }
}
