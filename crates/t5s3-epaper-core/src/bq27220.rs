//! Driver for the on-board BQ27220 battery fuel gauge.
//!
//! The chip shares the panel I2C bus owned by [`crate::i2c::Worker`], so
//! register access goes through this module's channel-based accessors
//! (queued as [`Request`]/[`Response`] and dispatched from the worker's
//! owning core) rather than this module owning the bus directly.

use esp_hal::{delay::Delay, i2c::master::I2c, Blocking};

pub(crate) const ADDR: u8 = 0x55;
const REG_CONTROL: u8 = 0x00;
const REG_VOLTAGE: u8 = 0x08;
const REG_BATTERY_STATUS: u8 = 0x0A;
const REG_REMAINING_CAPACITY: u8 = 0x10;
const REG_FULL_CHARGE_CAPACITY: u8 = 0x12;
const REG_TIME_TO_FULL: u8 = 0x18;
const REG_STATE_OF_CHARGE: u8 = 0x2C;
const REG_OPERATION_STATUS: u8 = 0x3A;
const REG_DESIGN_CAPACITY: u8 = 0x3C;
// manufacturer-access block: data-memory address at 0x3E/0x3F, data at
// 0x40.., committed by writing checksum + length together at 0x60/0x61.
const REG_MAC: u8 = 0x3E;
const REG_MAC_DATA: u8 = 0x40;
const REG_MAC_SUM: u8 = 0x60;
// battery status word: charge termination detected / battery detected.
const BATTERY_STATUS_FC: u16 = 1 << 9;
const BATTERY_STATUS_BATTPRES: u16 = 1 << 3;
// operation status word: gauge is in config-update mode (gauging suspended),
// plus the security-access bits (0b11 = sealed).
const OPERATION_STATUS_CFGUPDATE: u16 = 1 << 10;
const OPERATION_STATUS_SEC_MASK: u16 = 0b11 << 1;
const OPERATION_STATUS_SEC_SEALED: u16 = 0b11 << 1;
// enter config-update mode / exit it with a gauging re-initialization.
const SUBCMD_ENTER_CFG_UPDATE: u16 = 0x0090;
const SUBCMD_EXIT_CFG_UPDATE_REINIT: u16 = 0x0091;
// access keys: the sealed-to-unsealed pair, then the full-access key sent
// twice. data-memory writes need full access, not just unsealed.
const UNSEAL_KEY1: u16 = 0x0414;
const UNSEAL_KEY2: u16 = 0x3672;
const FULL_ACCESS_KEY: u16 = 0xFFFF;
// settle time after each access-key write.
const KEY_DELAY_MS: u32 = 5;
// CEDV profile 1 capacity words in data memory, stored big-endian.
const DM_FULL_CHARGE_CAPACITY: u16 = 0x929D;
const DM_DESIGN_CAPACITY: u16 = 0x929F;
// entering/leaving config-update mode can take a couple of seconds to show
// in the operation status register.
const CFG_POLL_TRIES: u32 = 30;
const CFG_POLL_INTERVAL_MS: u32 = 100;
// TimeToFull() reports this when the battery is not being charged.
const TIME_TO_FULL_NOT_CHARGING: u16 = 0xFFFF;

/// Diagnostic snapshot of the fuel gauge's capacity accounting.
#[derive(Clone, Copy, Debug)]
pub struct Diagnostics {
    /// Remaining capacity in mAh.
    pub remaining_mah: u16,
    /// Full-charge capacity in mAh: the gauge's 100% reference for the
    /// state-of-charge percentage.
    pub full_charge_mah: u16,
    /// Design capacity in mAh, as programmed in the gauge's data memory.
    pub design_mah: u16,
    /// The gauge itself has detected charge termination (FC flag).
    pub fully_charged: bool,
    /// The gauge has detected a battery (BATTPRES flag).
    pub battery_present: bool,
    /// The gauge is in config-update mode, which suspends gauging.
    pub config_update: bool,
    /// The gauge is sealed (configuration locked until unsealed).
    pub sealed: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Request {
    VoltageMv,
    StateOfCharge,
    TimeToFullMinutes,
    Diagnostics,
    ExitConfigUpdate,
    ProgramCapacity(u16),
}

pub(crate) enum Response {
    U16(crate::Result<u16>),
    OptU16(crate::Result<Option<u16>>),
    Diagnostics(crate::Result<Diagnostics>),
    Unit(crate::Result<()>),
}

/// Dispatch a queued request against the owned i2c bus. Called from
/// `crate::i2c::Worker::dispatch`, which runs on the core that owns `i2c`.
pub(crate) fn dispatch(i2c: &mut I2c<'_, Blocking>, request: Request) -> Response {
    match request {
        Request::VoltageMv => Response::U16(voltage_mv(i2c)),
        Request::StateOfCharge => Response::U16(state_of_charge(i2c)),
        Request::TimeToFullMinutes => Response::OptU16(time_to_full_minutes(i2c)),
        Request::Diagnostics => Response::Diagnostics(diagnostics(i2c)),
        Request::ExitConfigUpdate => Response::Unit(exit_config_update(i2c)),
        Request::ProgramCapacity(mah) => Response::Unit(program_capacity(i2c, mah)),
    }
}

pub(crate) fn battery_voltage_mv() -> crate::Result<u16> {
    match crate::i2c::submit(crate::i2c::Request::Battery(Request::VoltageMv)) {
        crate::i2c::Response::Battery(Response::U16(r)) => r,
        _ => unreachable!("bq27220: VoltageMv always answers Response::U16"),
    }
}

pub(crate) fn battery_state_of_charge() -> crate::Result<u16> {
    match crate::i2c::submit(crate::i2c::Request::Battery(Request::StateOfCharge)) {
        crate::i2c::Response::Battery(Response::U16(r)) => r,
        _ => unreachable!("bq27220: StateOfCharge always answers Response::U16"),
    }
}

pub(crate) fn battery_time_to_full_minutes() -> crate::Result<Option<u16>> {
    match crate::i2c::submit(crate::i2c::Request::Battery(Request::TimeToFullMinutes)) {
        crate::i2c::Response::Battery(Response::OptU16(r)) => r,
        _ => unreachable!("bq27220: TimeToFullMinutes always answers Response::OptU16"),
    }
}

pub(crate) fn fuel_gauge_diagnostics() -> crate::Result<Diagnostics> {
    match crate::i2c::submit(crate::i2c::Request::Battery(Request::Diagnostics)) {
        crate::i2c::Response::Battery(Response::Diagnostics(r)) => r,
        _ => unreachable!("bq27220: Diagnostics always answers Response::Diagnostics"),
    }
}

pub(crate) fn fuel_gauge_exit_config_update() -> crate::Result<()> {
    match crate::i2c::submit(crate::i2c::Request::Battery(Request::ExitConfigUpdate)) {
        crate::i2c::Response::Battery(Response::Unit(r)) => r,
        _ => unreachable!("bq27220: ExitConfigUpdate always answers Response::Unit"),
    }
}

pub(crate) fn fuel_gauge_program_capacity(capacity_mah: u16) -> crate::Result<()> {
    match crate::i2c::submit(crate::i2c::Request::Battery(Request::ProgramCapacity(
        capacity_mah,
    ))) {
        crate::i2c::Response::Battery(Response::Unit(r)) => r,
        _ => unreachable!("bq27220: ProgramCapacity always answers Response::Unit"),
    }
}

fn voltage_mv(i2c: &mut I2c<'_, Blocking>) -> crate::Result<u16> {
    read_word(i2c, REG_VOLTAGE)
}

fn state_of_charge(i2c: &mut I2c<'_, Blocking>) -> crate::Result<u16> {
    read_word(i2c, REG_STATE_OF_CHARGE)
}

// predicted minutes until full based on the average charge current, including
// the gauge's taper-time extension; None when the battery is not charging.
fn time_to_full_minutes(i2c: &mut I2c<'_, Blocking>) -> crate::Result<Option<u16>> {
    let minutes = read_word(i2c, REG_TIME_TO_FULL)?;
    if minutes == TIME_TO_FULL_NOT_CHARGING {
        Ok(None)
    } else {
        Ok(Some(minutes))
    }
}

fn diagnostics(i2c: &mut I2c<'_, Blocking>) -> crate::Result<Diagnostics> {
    let battery_status = read_word(i2c, REG_BATTERY_STATUS)?;
    let operation_status = read_word(i2c, REG_OPERATION_STATUS)?;
    Ok(Diagnostics {
        remaining_mah: read_word(i2c, REG_REMAINING_CAPACITY)?,
        full_charge_mah: read_word(i2c, REG_FULL_CHARGE_CAPACITY)?,
        design_mah: read_word(i2c, REG_DESIGN_CAPACITY)?,
        fully_charged: battery_status & BATTERY_STATUS_FC != 0,
        battery_present: battery_status & BATTERY_STATUS_BATTPRES != 0,
        config_update: operation_status & OPERATION_STATUS_CFGUPDATE != 0,
        sealed: operation_status & OPERATION_STATUS_SEC_MASK == OPERATION_STATUS_SEC_SEALED,
    })
}

// leave config-update mode with a gauging re-initialization. a gauge stranded
// in that mode (e.g. by an interrupted configuration write from a previous
// firmware) stops updating its state of charge entirely; the re-init also
// re-seeds the charge estimate from an open-circuit voltage measurement.
fn exit_config_update(i2c: &mut I2c<'_, Blocking>) -> crate::Result<()> {
    control(i2c, SUBCMD_EXIT_CFG_UPDATE_REINIT)
}

// program the CEDV profile's full-charge and design capacity to the real
// battery pack. the profile lives in RAM (reloaded from ROM defaults only if
// the gauge ever loses battery power), so callers re-apply this whenever the
// stored design capacity differs. exiting re-initializes gauging, re-seeding
// the charge estimate against the new capacity.
fn program_capacity(i2c: &mut I2c<'_, Blocking>, capacity_mah: u16) -> crate::Result<()> {
    let delay = Delay::new();

    // data-memory writes need full access: send the unseal key pair followed
    // by the full-access key twice. harmless when access is already open.
    for key in [UNSEAL_KEY1, UNSEAL_KEY2, FULL_ACCESS_KEY, FULL_ACCESS_KEY] {
        control(i2c, key)?;
        delay.delay_millis(KEY_DELAY_MS);
    }

    if read_word(i2c, REG_OPERATION_STATUS)? & OPERATION_STATUS_CFGUPDATE == 0 {
        control(i2c, SUBCMD_ENTER_CFG_UPDATE)?;
        wait_for_config_update(i2c, &delay, true)?;
    }

    // two separate block writes: a single 4-byte write starting at 0x929D
    // would cross the 32-byte data-memory block boundary at 0x92A0.
    write_data_memory_word(i2c, &delay, DM_FULL_CHARGE_CAPACITY, capacity_mah)?;
    write_data_memory_word(i2c, &delay, DM_DESIGN_CAPACITY, capacity_mah)?;

    control(i2c, SUBCMD_EXIT_CFG_UPDATE_REINIT)?;
    wait_for_config_update(i2c, &delay, false)?;

    // the re-init reloads the profile; confirm the write actually landed.
    if read_word(i2c, REG_DESIGN_CAPACITY)? != capacity_mah {
        return Err(crate::Error::GaugeCapacityMismatch);
    }
    Ok(())
}

fn wait_for_config_update(
    i2c: &mut I2c<'_, Blocking>,
    delay: &Delay,
    entered: bool,
) -> crate::Result<()> {
    let mut tries = 0;
    loop {
        let active = read_word(i2c, REG_OPERATION_STATUS)? & OPERATION_STATUS_CFGUPDATE != 0;
        if active == entered {
            return Ok(());
        }
        tries += 1;
        if tries >= CFG_POLL_TRIES {
            return Err(crate::Error::GaugeConfigTimeout);
        }
        delay.delay_millis(CFG_POLL_INTERVAL_MS);
    }
}

fn write_data_memory_word(
    i2c: &mut I2c<'_, Blocking>,
    delay: &Delay,
    address: u16,
    value: u16,
) -> crate::Result<()> {
    let address = address.to_le_bytes();
    let value = value.to_be_bytes();
    i2c.write(ADDR, &[REG_MAC, address[0], address[1], value[0], value[1]])
        .map_err(crate::Error::I2c)?;
    delay.delay_millis(1);

    // the block only commits once the checksum (complement of the summed
    // address and data bytes) and length are written together.
    let sum = address[0]
        .wrapping_add(address[1])
        .wrapping_add(value[0])
        .wrapping_add(value[1]);
    i2c.write(ADDR, &[REG_MAC_SUM, !sum, 6])
        .map_err(crate::Error::I2c)?;
    delay.delay_millis(100);

    // read the word back through the block interface while still in
    // config-update mode, so a rejected write fails at this step instead of
    // surfacing after the re-init.
    i2c.write(ADDR, &[REG_MAC, address[0], address[1]])
        .map_err(crate::Error::I2c)?;
    delay.delay_millis(1);
    let mut readback = [0u8; 2];
    i2c.write_read(ADDR, &[REG_MAC_DATA], &mut readback)
        .map_err(crate::Error::I2c)?;
    if readback != value {
        return Err(crate::Error::GaugeDataMemoryWrite);
    }
    Ok(())
}

fn control(i2c: &mut I2c<'_, Blocking>, subcommand: u16) -> crate::Result<()> {
    let subcommand = subcommand.to_le_bytes();
    i2c.write(ADDR, &[REG_CONTROL, subcommand[0], subcommand[1]])
        .map_err(crate::Error::I2c)
}

fn read_word(i2c: &mut I2c<'_, Blocking>, reg: u8) -> crate::Result<u16> {
    let mut value = [0u8; 2];
    i2c.write_read(ADDR, &[reg], &mut value)
        .map_err(crate::Error::I2c)?;
    Ok(u16::from_le_bytes(value))
}

impl crate::i2c::Registered for crate::i2c::Addr<{ ADDR }> {}
