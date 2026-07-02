use core::cell::RefCell;

use embedded_hal::{
    delay::DelayNs as _,
    spi::{Error as SpiError, ErrorKind, ErrorType, Operation, SpiBus, SpiDevice},
};
use esp_hal::{
    delay::Delay,
    gpio::Output,
    spi::master::{Config as SpiConfig, ConfigError, Spi},
    Blocking,
};

// one device on an SPI bus shared by reference. the bus (SPI2 plus the
// sclk/mosi/miso lines) is owned once by the caller inside a RefCell; each chip
// gets one of these, carrying its own chip-select and clock/mode. borrowing the
// bus through the RefCell is what serialises access: a second concurrent user
// panics instead of silently corrupting the bus.
pub(crate) struct SharedSpiDevice<'a, 'd> {
    bus: &'a RefCell<Spi<'d, Blocking>>,
    cs: Output<'d>,
    config: SpiConfig,
}

impl<'a, 'd> SharedSpiDevice<'a, 'd> {
    pub(crate) fn new(
        bus: &'a RefCell<Spi<'d, Blocking>>,
        cs: Output<'d>,
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

        self.cs.set_low();
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
        self.cs.set_high();

        op_res.and(flush_res).map_err(Error::Spi)
    }
}
