use core::cell::RefCell;

use esp_hal::{
    i2c::master::{Config as I2cConfig, ConfigError, I2c},
    peripherals,
    time::Rate,
    Blocking,
};

const FREQUENCY_KHZ: u32 = 100;

/// The shared I2C0 bus.
///
/// Every on-board I2C peripheral (PCA9555 IO expander, TPS65185 panel PMIC,
/// GT911 touch controller, BQ27220 fuel gauge, BQ25896 charger) lives on this
/// one bus. Owning it once and lending it out by reference is what lets the
/// display, the input controller, and any future peripheral (e.g. the
/// PCF85063 external RTC) coexist without threading everything through one
/// driver.
pub struct Bus<'d> {
    pub(crate) i2c: RefCell<I2c<'d, Blocking>>,
}

impl<'d> Bus<'d> {
    /// Build the shared bus on I2C0. The returned bus must outlive every
    /// driver created from it.
    pub fn new(
        i2c: peripherals::I2C0<'d>,
        sda: peripherals::GPIO39<'d>,
        scl: peripherals::GPIO40<'d>,
    ) -> core::result::Result<Self, ConfigError> {
        let i2c = I2c::new(
            i2c,
            I2cConfig::default().with_frequency(Rate::from_khz(FREQUENCY_KHZ)),
        )?
        .with_sda(sda)
        .with_scl(scl);
        Ok(Self {
            i2c: RefCell::new(i2c),
        })
    }
}
