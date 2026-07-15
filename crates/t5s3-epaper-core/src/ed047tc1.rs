use esp_hal::{
    dma::DmaTxBuf,
    dma_buffers,
    gpio::{Level, Output, OutputConfig},
    lcd_cam::{
        lcd::{i8080, i8080::Command},
        LcdCam,
    },
    peripherals,
    rmt::PulseCode,
    time::Rate,
    Blocking,
};

use crate::{
    pca9555::{ADDR as PCA9555_ADDR, REG_INPUT_PORT1 as PCA9555_REG_INPUT_PORT1},
    rmt,
};

macro_rules! pulse {
    ($high:expr, $low:expr) => {
        if $high > 0 {
            [
                PulseCode::new(Level::High, $high, Level::Low, $low),
                PulseCode::end_marker(),
            ]
        } else {
            [
                PulseCode::new(Level::High, $low, Level::Low, 0),
                PulseCode::end_marker(),
            ]
        }
    };
}

const DMA_BUFFER_SIZE: usize = 248;
pub(crate) const TPS65185_ADDR: u8 = 0x68;
const PCA9555_REG_OUTPUT_PORT0: u8 = 2;
const PCA9555_REG_OUTPUT_PORT1: u8 = 3;
const PCA9555_REG_INVERT_PORT0: u8 = 4;
const PCA9555_REG_INVERT_PORT1: u8 = 5;
const PCA9555_REG_CONFIG_PORT0: u8 = 6;
const PCA9555_REG_CONFIG_PORT1: u8 = 7;
const TPS_REG_TMST_VALUE: u8 = 0x00;
const TPS_REG_ENABLE: u8 = 0x01;
const TPS_REG_VCOM1: u8 = 0x03;
const TPS_REG_VCOM2: u8 = 0x04;
const TPS_REG_TMST1: u8 = 0x0D;
const TPS_REG_PG: u8 = 0x0F;
const TPS_TMST1_READ_THERM: u8 = 1 << 7;
const VCOM_MV: u16 = 1600;
// port 0 bit 0 gates the GPS/LoRa 3.3 V rail (active high); matches the factory
// firmware's `io_extend_lora_gps_power_on`.
const PCA_BIT_LORA_GPS_PWR: u8 = 1 << 0;
const PCA_BIT_OE: u8 = 1 << 0;
const PCA_BIT_MODE: u8 = 1 << 1;
const PCA_BIT_BUTTON: u8 = 1 << 2;
const PCA_BIT_PWRUP: u8 = 1 << 3;
const PCA_BIT_VCOM_CTRL: u8 = 1 << 4;
const PCA_BIT_WAKEUP: u8 = 1 << 5;
const PCA_BIT_PWRGOOD: u8 = 1 << 6;
const PCA_BIT_INT: u8 = 1 << 7;

#[derive(Default)]
struct ConfigRegister {
    mode: bool,
    output_enable: bool,
    pwrup: bool,
    vcom_ctrl: bool,
    wakeup: bool,
}

struct ConfigWriter<'d> {
    leh: Output<'d>,
    stv: Output<'d>,
    output_port0: u8,
    output_port1: u8,
    config: ConfigRegister,
}

impl<'d> ConfigWriter<'d> {
    fn new(leh: peripherals::GPIO42<'d>, stv: peripherals::GPIO45<'d>) -> crate::Result<Self> {
        let mut writer = ConfigWriter {
            leh: Output::new(leh, Level::Low, OutputConfig::default()),
            stv: Output::new(stv, Level::High, OutputConfig::default()),
            output_port0: 0xFF,
            output_port1: 0,
            config: ConfigRegister::default(),
        };

        writer.write_register(
            PCA9555_ADDR,
            &[
                PCA9555_REG_CONFIG_PORT1,
                PCA_BIT_BUTTON | PCA_BIT_PWRGOOD | PCA_BIT_INT,
            ],
        )?;
        writer.write_register(PCA9555_ADDR, &[PCA9555_REG_INVERT_PORT0, 0x00])?;
        writer.write_register(PCA9555_ADDR, &[PCA9555_REG_INVERT_PORT1, 0x00])?;
        writer.write_register(PCA9555_ADDR, &[PCA9555_REG_CONFIG_PORT0, 0x00])?;
        writer.write_register(
            PCA9555_ADDR,
            &[PCA9555_REG_OUTPUT_PORT0, writer.output_port0],
        )?;
        writer.write()?;

        Ok(writer)
    }

    fn write(&mut self) -> crate::Result<()> {
        let mut value = 0;
        if self.config.output_enable {
            value |= PCA_BIT_OE;
        }
        if self.config.mode {
            value |= PCA_BIT_MODE;
        }
        if self.config.pwrup {
            value |= PCA_BIT_PWRUP;
        }
        if self.config.vcom_ctrl {
            value |= PCA_BIT_VCOM_CTRL;
        }
        if self.config.wakeup {
            value |= PCA_BIT_WAKEUP;
        }
        self.output_port1 = value;
        self.write_register(PCA9555_ADDR, &[PCA9555_REG_OUTPUT_PORT1, value])
    }

    fn set_lora_gps_power(&mut self, on: bool) -> crate::Result<()> {
        if on {
            self.output_port0 |= PCA_BIT_LORA_GPS_PWR;
        } else {
            self.output_port0 &= !PCA_BIT_LORA_GPS_PWR;
        }
        self.write_register(PCA9555_ADDR, &[PCA9555_REG_OUTPUT_PORT0, self.output_port0])
    }

    fn set_stv(&mut self, level: bool) {
        self.stv
            .set_level(if level { Level::High } else { Level::Low });
    }

    fn pulse_leh(&mut self) {
        self.leh.set_high();
        busy_delay(64);
        self.leh.set_low();
        busy_delay(64);
    }

    fn pwrgood(&mut self) -> crate::Result<bool> {
        Ok(self.read_register(PCA9555_ADDR, PCA9555_REG_INPUT_PORT1)? & PCA_BIT_PWRGOOD != 0)
    }

    fn enable_tps(&mut self) -> crate::Result<()> {
        self.write_register(TPS65185_ADDR, &[TPS_REG_ENABLE, 0x3F])?;
        self.set_vcom(VCOM_MV)
    }

    fn set_vcom(&mut self, mv: u16) -> crate::Result<()> {
        let value = mv / 10;
        self.write_register(
            TPS65185_ADDR,
            &[TPS_REG_VCOM2, ((value & 0x100) >> 8) as u8],
        )?;
        self.write_register(TPS65185_ADDR, &[TPS_REG_VCOM1, (value & 0xFF) as u8])
    }

    fn tps_power_good(&mut self) -> crate::Result<bool> {
        Ok(self.read_register(TPS65185_ADDR, TPS_REG_PG)? & 0xFA == 0xFA)
    }

    fn panel_temperature(&mut self) -> crate::Result<i8> {
        self.write_register(TPS65185_ADDR, &[TPS_REG_TMST1, TPS_TMST1_READ_THERM])?;
        // poll CONV_END (bit 5) in TMST1 until conversion finishes
        let mut tries = 0;
        loop {
            let tmst1 = self.read_register(TPS65185_ADDR, TPS_REG_TMST1)?;
            if tmst1 & (1 << 5) != 0 {
                break;
            }
            tries += 1;
            if tries >= 500 {
                return Err(crate::Error::PowerTimeout);
            }
            busy_delay(240_000);
        }
        let raw = self.read_register(TPS65185_ADDR, TPS_REG_TMST_VALUE)?;
        Ok(raw as i8)
    }

    fn read_register(&mut self, device: u8, reg: u8) -> crate::Result<u8> {
        crate::i2c::read_byte(device, reg)
    }

    fn write_register(&mut self, device: u8, payload: &[u8]) -> crate::Result<()> {
        debug_assert_eq!(payload.len(), 2, "PCA9555/TPS65185 registers are 8-bit");
        crate::i2c::write_byte(device, payload[0], payload[1])
    }

    fn battery_voltage_mv(&mut self) -> crate::Result<u16> {
        crate::bq27220::battery_voltage_mv()
    }

    fn battery_state_of_charge(&mut self) -> crate::Result<u16> {
        crate::bq27220::battery_state_of_charge()
    }

    fn battery_time_to_full_minutes(&mut self) -> crate::Result<Option<u16>> {
        crate::bq27220::battery_time_to_full_minutes()
    }

    fn fuel_gauge_diagnostics(&mut self) -> crate::Result<crate::bq27220::Diagnostics> {
        crate::bq27220::fuel_gauge_diagnostics()
    }

    fn fuel_gauge_exit_config_update(&mut self) -> crate::Result<()> {
        crate::bq27220::fuel_gauge_exit_config_update()
    }

    fn fuel_gauge_program_capacity(&mut self, capacity_mah: u16) -> crate::Result<()> {
        crate::bq27220::fuel_gauge_program_capacity(capacity_mah)
    }

    fn shutdown(&mut self) -> crate::Result<()> {
        crate::bq25896::charger_shutdown()
    }

    fn charger_status(&mut self) -> crate::Result<crate::bq25896::Status> {
        crate::bq25896::charger_status()
    }
}

impl crate::i2c::Registered for crate::i2c::Addr<{ TPS65185_ADDR }> {}

pub struct PinConfig<'a> {
    pub data0: peripherals::GPIO5<'a>,
    pub data1: peripherals::GPIO6<'a>,
    pub data2: peripherals::GPIO7<'a>,
    pub data3: peripherals::GPIO15<'a>,
    pub data4: peripherals::GPIO16<'a>,
    pub data5: peripherals::GPIO17<'a>,
    pub data6: peripherals::GPIO18<'a>,
    pub data7: peripherals::GPIO8<'a>,
    pub leh: peripherals::GPIO42<'a>,
    pub lcd_dc: peripherals::GPIO41<'a>,
    pub lcd_wrx: peripherals::GPIO4<'a>,
    pub rmt: peripherals::GPIO48<'a>,
    pub stv: peripherals::GPIO45<'a>,
}

pub(crate) struct ED047TC1<'d> {
    i8080: Option<i8080::I8080<'d, Blocking>>,
    cfg_writer: ConfigWriter<'d>,
    rmt: rmt::Rmt<'d>,
    dma_buf: Option<DmaTxBuf>,
}

impl<'d> ED047TC1<'d> {
    // PCA9555 register writes here go through the channel to core 1's i2c
    // worker (see `crate::i2c`), which must already be running by this point
    // — this is called after `CpuControl::start_app_core` spawns it, not
    // before.
    pub(crate) fn new(
        pins: PinConfig<'d>,
        dma: peripherals::DMA_CH0<'d>,
        lcd_cam: peripherals::LCD_CAM<'d>,
        rmt: peripherals::RMT<'d>,
    ) -> crate::Result<Self> {
        let lcd_cam = LcdCam::new(lcd_cam);

        let mut cfg_writer = ConfigWriter::new(pins.leh, pins.stv)?;
        cfg_writer.write()?;

        let (_, _, tx_buffer, tx_descriptors) = dma_buffers!(0, DMA_BUFFER_SIZE);
        let dma_buf =
            Some(DmaTxBuf::new(tx_descriptors, tx_buffer).map_err(crate::Error::DmaBuffer)?);

        let config = i8080::Config::default()
            .with_frequency(Rate::from_mhz(20))
            .with_cd_idle_edge(false)
            .with_cd_cmd_edge(true)
            .with_cd_dummy_edge(false)
            .with_cd_data_edge(false);
        let ctrl = ED047TC1 {
            i8080: Some(
                i8080::I8080::new(lcd_cam.lcd, dma, config)
                    .map_err(crate::Error::I8080)?
                    .with_dc(pins.lcd_dc)
                    .with_wrx(pins.lcd_wrx)
                    .with_data0(pins.data6)
                    .with_data1(pins.data7)
                    .with_data2(pins.data4)
                    .with_data3(pins.data5)
                    .with_data4(pins.data2)
                    .with_data5(pins.data3)
                    .with_data6(pins.data0)
                    .with_data7(pins.data1),
            ),
            cfg_writer,
            rmt: rmt::Rmt::new(rmt, pins.rmt),
            dma_buf,
        };
        Ok(ctrl)
    }

    pub(crate) fn power_on(&mut self) -> crate::Result<()> {
        self.cfg_writer.set_stv(true);
        self.cfg_writer.config.output_enable = true;
        self.cfg_writer.config.mode = false;
        self.cfg_writer.config.wakeup = true;
        self.cfg_writer.write()?;
        self.cfg_writer.config.pwrup = true;
        self.cfg_writer.write()?;
        self.cfg_writer.config.vcom_ctrl = true;
        self.cfg_writer.write()?;
        busy_delay(240_000);
        let mut tries = 0;
        while !self.cfg_writer.pwrgood()? {
            tries += 1;
            if tries >= 500 {
                return Err(crate::Error::PowerTimeout);
            }
            busy_delay(240_000);
        }
        self.cfg_writer.enable_tps()?;
        let mut tries = 0;
        while !self.cfg_writer.tps_power_good()? {
            tries += 1;
            if tries >= 500 {
                return Err(crate::Error::PowerTimeout);
            }
            busy_delay(240_000);
        }
        Ok(())
    }

    pub(crate) fn power_off(&mut self) -> crate::Result<()> {
        self.cfg_writer.config.vcom_ctrl = false;
        self.cfg_writer.config.pwrup = false;
        self.cfg_writer.config.output_enable = false;
        self.cfg_writer.config.mode = false;
        self.cfg_writer.write()?;
        busy_delay(240_000);
        self.cfg_writer.config.wakeup = false;
        self.cfg_writer.write()?;
        self.cfg_writer.set_stv(false);
        Ok(())
    }

    pub(crate) fn battery_voltage_mv(&mut self) -> crate::Result<u16> {
        self.cfg_writer.battery_voltage_mv()
    }

    pub(crate) fn battery_state_of_charge(&mut self) -> crate::Result<u16> {
        self.cfg_writer.battery_state_of_charge()
    }

    pub(crate) fn charger_status(&mut self) -> crate::Result<crate::bq25896::Status> {
        self.cfg_writer.charger_status()
    }

    pub(crate) fn battery_time_to_full_minutes(&mut self) -> crate::Result<Option<u16>> {
        self.cfg_writer.battery_time_to_full_minutes()
    }

    pub(crate) fn fuel_gauge_diagnostics(&mut self) -> crate::Result<crate::bq27220::Diagnostics> {
        self.cfg_writer.fuel_gauge_diagnostics()
    }

    pub(crate) fn fuel_gauge_exit_config_update(&mut self) -> crate::Result<()> {
        self.cfg_writer.fuel_gauge_exit_config_update()
    }

    pub(crate) fn fuel_gauge_program_capacity(&mut self, capacity_mah: u16) -> crate::Result<()> {
        self.cfg_writer.fuel_gauge_program_capacity(capacity_mah)
    }

    pub(crate) fn panel_temperature(&mut self) -> crate::Result<i8> {
        self.cfg_writer.panel_temperature()
    }

    pub(crate) fn shutdown(&mut self) -> crate::Result<()> {
        self.cfg_writer.shutdown()
    }

    pub(crate) fn lora_gps_power_off(&mut self) -> crate::Result<()> {
        self.cfg_writer.set_lora_gps_power(false)
    }

    pub(crate) fn frame_start(&mut self) -> crate::Result<()> {
        self.cfg_writer.config.mode = true;
        self.cfg_writer.write()?;

        let data = pulse!(10, 10);
        self.rmt.pulse(&data, true)?;

        self.cfg_writer.set_stv(false);

        busy_delay(240);
        let data = pulse!(100, 100);
        let rmt_tx = self.rmt.pulse(&data, false)?;

        self.cfg_writer.set_stv(true);

        if let Some(rmt_tx) = rmt_tx {
            self.rmt.reclaim_channel(rmt_tx)?;
        }

        let data = pulse!(0, 100);
        self.rmt.pulse(&data, true)?;

        self.cfg_writer.config.output_enable = true;
        self.cfg_writer.write()?;

        let data = pulse!(10, 10);
        self.rmt.pulse(&data, true)?;

        Ok(())
    }

    pub(crate) fn latch_row(&mut self) {
        self.cfg_writer.pulse_leh();
    }

    pub(crate) fn skip(&mut self) -> crate::Result<()> {
        let data = pulse!(45, 5);
        if let Some(rmt_tx) = self.rmt.pulse(&data, false)? {
            self.rmt.reclaim_channel(rmt_tx)?;
        }
        Ok(())
    }

    pub(crate) fn output_row(&mut self, output_time: u16) -> crate::Result<()> {
        self.latch_row();

        // take the display resources before starting the row pulse so a missing
        // handle can't strand the rmt channel the pulse claims below.
        let i8080 = self.i8080.take().ok_or(crate::Error::MissingI8080)?;
        let dma_buf = match self.dma_buf.take() {
            Some(dma_buf) => dma_buf,
            None => {
                self.i8080 = Some(i8080);
                return Err(crate::Error::MissingDmaBuffer);
            }
        };

        let data = pulse!(output_time, 50);
        let rmt_tx = match self.rmt.pulse(&data, false) {
            Ok(rmt_tx) => rmt_tx,
            Err(e) => {
                self.i8080 = Some(i8080);
                self.dma_buf = Some(dma_buf);
                return Err(e);
            }
        };

        let tx = match i8080.send(Command::<u8>::One(0), 0, dma_buf) {
            Ok(tx) => tx,
            Err((err, i8080, buf)) => {
                self.i8080 = Some(i8080);
                self.dma_buf = Some(buf);
                // reclaim the channel the row pulse already claimed (best effort;
                // we are already returning the send error).
                if let Some(rmt_tx) = rmt_tx {
                    self.rmt.reclaim_channel(rmt_tx).ok();
                }
                return Err(crate::Error::Dma(err));
            }
        };

        let (r, i8080, dma_buf) = tx.wait();
        // restore the display resources before the fallible reclaim / result
        // checks so an error there can't strand them either.
        self.i8080 = Some(i8080);
        self.dma_buf = Some(dma_buf);
        if let Some(rmt_tx) = rmt_tx {
            self.rmt.reclaim_channel(rmt_tx)?;
        }
        r.map_err(crate::Error::Dma)?;

        Ok(())
    }

    pub(crate) fn frame_end(&mut self) -> crate::Result<()> {
        self.cfg_writer.config.output_enable = false;
        self.cfg_writer.write()?;
        self.cfg_writer.config.mode = false;
        self.cfg_writer.write()?;
        let data = pulse!(10, 10);
        self.rmt.pulse(&data, true)?;
        self.rmt.pulse(&data, true)?;

        Ok(())
    }

    pub(crate) fn set_buffer(&mut self, data: &[u8]) -> crate::Result<()> {
        let mut dma_buf = self.dma_buf.take().ok_or(crate::Error::MissingDmaBuffer)?;
        dma_buf.as_mut_slice().fill(0);
        dma_buf.as_mut_slice()[..data.len()].copy_from_slice(data);
        self.dma_buf = Some(dma_buf);
        Ok(())
    }
}

#[inline(always)]
pub(crate) fn busy_delay(wait_cycles: u32) {
    // compare elapsed cycles with wrapping subtraction: the 32-bit cycle
    // counter wraps roughly every ~18s at 240MHz, so a plain `start + wait`
    // target can sit above any value the counter reaches and spin forever.
    let start = cycles();
    while cycles().wrapping_sub(start) < wait_cycles {}
}

#[inline(always)]
fn cycles() -> u32 {
    esp_hal::xtensa_lx::timer::get_cycle_count()
}
