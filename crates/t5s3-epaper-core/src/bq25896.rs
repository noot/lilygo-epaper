//! Driver for the on-board BQ25896 battery charger PMIC.
//!
//! The chip shares the panel I2C bus owned by the display driver, so register
//! access goes through [`crate::Display::charger_status`] and
//! [`crate::power::shutdown`] rather than this module owning the bus.

use esp_hal::{delay::Delay, i2c::master::I2c, Blocking};

const ADDR: u8 = 0x6B;
const REG_ADC_CONTROL: u8 = 0x02;
const REG_MISC_OPERATION: u8 = 0x09;
const REG_STATUS: u8 = 0x0B;
const REG_VBUS_VOLTAGE: u8 = 0x11;
const REG_CHARGE_CURRENT: u8 = 0x12;
const ADC_CONV_START: u8 = 1 << 7;
const ADC_CONV_RATE: u8 = 1 << 6;
const BATFET_DIS: u8 = 1 << 5;
// a one-shot conversion typically finishes in ~10 ms; poll up to 500 ms.
const ADC_POLL_TRIES: u32 = 100;
const ADC_POLL_INTERVAL_MS: u32 = 5;

/// Charging phase reported by the charger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChargeStatus {
    /// Not charging.
    NotCharging,
    /// Pre-charge phase (battery below the fast-charge threshold).
    PreCharge,
    /// Fast charge phase (constant current / constant voltage).
    FastCharge,
    /// Charge cycle finished.
    Done,
}

/// Input source detected on VBUS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VbusStatus {
    /// Nothing attached; running from the battery.
    NoInput,
    /// Standard USB host port.
    UsbHost,
    /// Dedicated charging adapter.
    Adapter,
    /// Boost (OTG) output is active.
    Otg,
    /// Reserved value reported by the chip.
    Unknown,
}

/// Decoded snapshot of the charger status registers.
#[derive(Clone, Copy, Debug)]
pub struct Status {
    /// Charging phase.
    pub charge: ChargeStatus,
    /// Input source attached to VBUS.
    pub vbus: VbusStatus,
    /// Measured VBUS voltage in millivolts (0 when no input is attached).
    pub vbus_mv: u16,
    /// Measured charge current in milliamps.
    pub charge_ma: u16,
}

// kick a one-shot ADC conversion so the voltage/current registers hold fresh
// values, wait for it to finish, then decode the status registers.
pub(crate) fn read_status(i2c: &mut I2c<'_, Blocking>) -> crate::Result<Status> {
    // CONV_RATE is cleared in case continuous mode was left enabled, so that
    // CONV_START reliably self-clears when the conversion completes.
    let adc = read_register(i2c, REG_ADC_CONTROL)?;
    write_register(
        i2c,
        REG_ADC_CONTROL,
        (adc | ADC_CONV_START) & !ADC_CONV_RATE,
    )?;

    let delay = Delay::new();
    let mut tries = 0;
    while read_register(i2c, REG_ADC_CONTROL)? & ADC_CONV_START != 0 {
        tries += 1;
        if tries >= ADC_POLL_TRIES {
            return Err(crate::Error::ChargerAdcTimeout);
        }
        delay.delay_millis(ADC_POLL_INTERVAL_MS);
    }

    let status = read_register(i2c, REG_STATUS)?;
    let vbus_raw = read_register(i2c, REG_VBUS_VOLTAGE)? & 0x7F;
    let charge_raw = read_register(i2c, REG_CHARGE_CURRENT)? & 0x7F;

    Ok(Status {
        charge: match (status >> 3) & 0x03 {
            0 => ChargeStatus::NotCharging,
            1 => ChargeStatus::PreCharge,
            2 => ChargeStatus::FastCharge,
            _ => ChargeStatus::Done,
        },
        vbus: match (status >> 5) & 0x07 {
            0 => VbusStatus::NoInput,
            1 => VbusStatus::UsbHost,
            2 => VbusStatus::Adapter,
            7 => VbusStatus::Otg,
            _ => VbusStatus::Unknown,
        },
        // the VBUS ADC floors at its 2.6 V offset; a raw zero means nothing
        // is attached, so report 0 instead of the offset.
        vbus_mv: if vbus_raw == 0 {
            0
        } else {
            2600 + u16::from(vbus_raw) * 100
        },
        charge_ma: u16::from(charge_raw) * 50,
    })
}

// turn the BATFET off, disconnecting the battery from the system rail. this is
// the full power-off path used by `power::shutdown`.
pub(crate) fn disable_batfet(i2c: &mut I2c<'_, Blocking>) -> crate::Result<()> {
    let value = read_register(i2c, REG_MISC_OPERATION)?;
    write_register(i2c, REG_MISC_OPERATION, value | BATFET_DIS)
}

fn read_register(i2c: &mut I2c<'_, Blocking>, reg: u8) -> crate::Result<u8> {
    let mut value = [0u8; 1];
    i2c.write_read(ADDR, &[reg], &mut value)
        .map_err(crate::Error::I2c)?;
    Ok(value[0])
}

fn write_register(i2c: &mut I2c<'_, Blocking>, reg: u8, value: u8) -> crate::Result<()> {
    i2c.write(ADDR, &[reg, value]).map_err(crate::Error::I2c)
}
