use esp_hal::{
    dma::DmaTxBuf,
    dma_buffers,
    gpio::{Flex, Input, InputConfig, Level, Output, OutputConfig, Pull, RtcPin},
    i2c::master::{Config as I2cConfig, I2c},
    lcd_cam::{
        lcd::{i8080, i8080::Command},
        LcdCam,
    },
    peripherals,
    rmt::PulseCode,
    time::Rate,
    Blocking,
};
use log::debug;

use crate::{input::{Buttons, InputState}, rmt, touchscreen::TouchState};

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
const I2C_FREQUENCY_KHZ: u32 = 100;
const PCA9555_ADDR: u8 = 0x20;
const TPS65185_ADDR: u8 = 0x68;
const PCA9555_REG_INPUT_PORT1: u8 = 1;
const PCA9555_REG_OUTPUT_PORT0: u8 = 2;
const PCA9555_REG_OUTPUT_PORT1: u8 = 3;
const PCA9555_REG_INVERT_PORT0: u8 = 4;
const PCA9555_REG_INVERT_PORT1: u8 = 5;
const PCA9555_REG_CONFIG_PORT0: u8 = 6;
const PCA9555_REG_CONFIG_PORT1: u8 = 7;
const TPS_REG_ENABLE: u8 = 0x01;
const TPS_REG_VCOM1: u8 = 0x03;
const TPS_REG_VCOM2: u8 = 0x04;
const TPS_REG_PG: u8 = 0x0F;
const BQ27220_ADDR: u8 = 0x55;
const BQ27220_REG_VOLTAGE: u8 = 0x08;
const BQ27220_REG_STATE_OF_CHARGE: u8 = 0x2C;
const GT911_ADDR_LOW: u8 = 0x5D;
const GT911_ADDR_HIGH: u8 = 0x14;
const GT911_PRODUCT_ID: u16 = 0x8140;
const GT911_CONFIG_VERSION: u16 = 0x8047;
const GT911_MODULE_SWITCH_1: u16 = 0x804D;
const GT911_CONFIG_CHKSUM: u16 = 0x80FF;
const GT911_CONFIG_FRESH: u16 = 0x8100;
const GT911_CONFIG_LENGTH: usize = 186;
const GT911_POINT_INFO: u16 = 0x814E;
const GT911_POINT_1: u16 = 0x814F;
const GT911_X_RESOLUTION: u16 = 0x8146;
const GT911_Y_RESOLUTION: u16 = 0x8148;
const GT911_DEV_ID: u32 = 911;
const VCOM_MV: u16 = 1600;
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

struct ConfigWriter<'a> {
    i2c: I2c<'a, Blocking>,
    leh: Output<'a>,
    stv: Output<'a>,
    touch_rst: Output<'a>,
    touch_int: Flex<'a>,
    touch_initialized: bool,
    touch_addr: u8,
    touch_resolution: (u16, u16),
    output_port1: u8,
    config: ConfigRegister,
}

impl<'a> ConfigWriter<'a> {
    fn new(
        i2c: peripherals::I2C0<'a>,
        sda: peripherals::GPIO39<'a>,
        scl: peripherals::GPIO40<'a>,
        leh: peripherals::GPIO42<'a>,
        stv: peripherals::GPIO45<'a>,
        touch_rst: peripherals::GPIO9<'a>,
        touch_int: peripherals::GPIO3<'a>,
    ) -> crate::Result<Self> {
        let i2c = I2c::new(
            i2c,
            I2cConfig::default().with_frequency(Rate::from_khz(I2C_FREQUENCY_KHZ)),
        )
        .map_err(crate::Error::I2cConfig)?
        .with_sda(sda)
        .with_scl(scl);

        touch_rst.rtcio_pad_hold(false);
        touch_int.rtcio_pad_hold(false);

        let mut writer = ConfigWriter {
            i2c,
            leh: Output::new(leh, Level::Low, OutputConfig::default()),
            stv: Output::new(stv, Level::High, OutputConfig::default()),
            touch_rst: Output::new(touch_rst, Level::High, OutputConfig::default()),
            touch_int: {
                let mut pin = Flex::new(touch_int);
                pin.set_output_enable(false);
                pin.set_input_enable(true);
                pin.apply_input_config(&InputConfig::default());
                pin
            },
            touch_initialized: false,
            touch_addr: GT911_ADDR_LOW,
            touch_resolution: (0, 0),
            output_port1: 0,
            config: ConfigRegister::default(),
        };

        writer.write_register(
            PCA9555_ADDR,
            &[PCA9555_REG_CONFIG_PORT1, PCA_BIT_BUTTON | PCA_BIT_PWRGOOD | PCA_BIT_INT],
        )?;
        writer.write_register(PCA9555_ADDR, &[PCA9555_REG_INVERT_PORT0, 0x00])?;
        writer.write_register(PCA9555_ADDR, &[PCA9555_REG_INVERT_PORT1, 0x00])?;
        writer.write_register(PCA9555_ADDR, &[PCA9555_REG_CONFIG_PORT0, 0x00])?;
        writer.write_register(PCA9555_ADDR, &[PCA9555_REG_OUTPUT_PORT0, 0xFF])?;
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

    fn set_stv(&mut self, level: bool) {
        self.stv.set_level(if level { Level::High } else { Level::Low });
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
        self.write_register(TPS65185_ADDR, &[TPS_REG_VCOM2, ((value & 0x100) >> 8) as u8])?;
        self.write_register(TPS65185_ADDR, &[TPS_REG_VCOM1, (value & 0xFF) as u8])
    }

    fn tps_power_good(&mut self) -> crate::Result<bool> {
        Ok(self.read_register(TPS65185_ADDR, TPS_REG_PG)? & 0xFA == 0xFA)
    }

    fn read_register(&mut self, device: u8, reg: u8) -> crate::Result<u8> {
        let mut value = [0u8; 1];
        self.i2c
            .write_read(device, &[reg], &mut value)
            .map_err(crate::Error::I2c)?;
        Ok(value[0])
    }

    fn read_register_u16(&mut self, device: u8, reg: u8) -> crate::Result<u16> {
        let mut value = [0u8; 2];
        self.i2c
            .write_read(device, &[reg], &mut value)
            .map_err(crate::Error::I2c)?;
        Ok(u16::from_le_bytes(value))
    }

    fn write_register(&mut self, device: u8, payload: &[u8]) -> crate::Result<()> {
        self.i2c.write(device, payload).map_err(crate::Error::I2c)
    }

    fn write_register16(&mut self, device: u8, reg: u16, payload: &[u8]) -> crate::Result<()> {
        let mut buffer = [0u8; 41];
        let len = payload.len() + 2;
        buffer[0] = (reg >> 8) as u8;
        buffer[1] = reg as u8;
        buffer[2..len].copy_from_slice(payload);
        self.i2c.write(device, &buffer[..len]).map_err(crate::Error::I2c)
    }

    fn read_register16(&mut self, device: u8, reg: u16, payload: &mut [u8]) -> crate::Result<()> {
        let reg = [(reg >> 8) as u8, reg as u8];
        self.i2c
            .write_read(device, &reg, payload)
            .map_err(crate::Error::I2c)
    }

    fn battery_voltage_mv(&mut self) -> crate::Result<u16> {
        self.read_register_u16(BQ27220_ADDR, BQ27220_REG_VOLTAGE)
    }

    fn battery_state_of_charge(&mut self) -> crate::Result<u16> {
        self.read_register_u16(BQ27220_ADDR, BQ27220_REG_STATE_OF_CHARGE)
    }

    fn auxiliary_button_pressed(&mut self) -> crate::Result<bool> {
        Ok(self.read_register(PCA9555_ADDR, PCA9555_REG_INPUT_PORT1)? & PCA_BIT_BUTTON == 0)
    }

    fn init_touch(&mut self) -> crate::Result<()> {
        debug!("touch init: probing GT911");
        self.touch_reset_for_address(GT911_ADDR_LOW)?;

        let mut product_id = [0u8; 4];
        if self
            .read_register16(GT911_ADDR_LOW, GT911_PRODUCT_ID, &mut product_id)
            .is_ok()
            && parse_gt911_chip_id(product_id) == GT911_DEV_ID
        {
            debug!(
                "touch init: addr 0x{:02X} product_id={:?}",
                GT911_ADDR_LOW, product_id
            );
            self.touch_addr = GT911_ADDR_LOW;
        } else {
            debug!(
                "touch init: addr 0x{:02X} probe failed product_id={:?}",
                GT911_ADDR_LOW, product_id
            );
            self.touch_reset_for_address(GT911_ADDR_HIGH)?;
            self.read_register16(GT911_ADDR_HIGH, GT911_PRODUCT_ID, &mut product_id)?;
            debug!(
                "touch init: addr 0x{:02X} product_id={:?}",
                GT911_ADDR_HIGH, product_id
            );
            self.touch_addr = GT911_ADDR_HIGH;
        }

        let chip_id = parse_gt911_chip_id(product_id);
        if chip_id != GT911_DEV_ID {
            debug!("touch init: unexpected chip id {}", chip_id);
            return Err(crate::Error::TouchInitFailed);
        }

        self.touch_resolution = (
            self.touch_read_u16(GT911_X_RESOLUTION)?,
            self.touch_read_u16(GT911_Y_RESOLUTION)?,
        );
        self.touch_set_interrupt_mode_low_level_query()?;
        debug!(
            "touch init: resolution={}x{}",
            self.touch_resolution.0, self.touch_resolution.1
        );
        self.touch_initialized = true;

        Ok(())
    }

    fn ensure_touch(&mut self) -> crate::Result<()> {
        if self.touch_initialized {
            return Ok(());
        }
        debug!("touch init: lazy init");
        self.init_touch()
    }

    fn touch_reset_for_address(&mut self, address: u8) -> crate::Result<()> {
        self.touch_rst.set_low();
        busy_delay(30_000);

        match address {
            GT911_ADDR_HIGH => {
                self.touch_int.set_high();
            }
            _ => {
                self.touch_int.set_low();
            }
        }
        self.touch_int.set_output_enable(true);
        self.touch_int.set_input_enable(false);
        busy_delay(30_000);
        self.touch_rst.set_high();
        busy_delay(4_500_000);
        self.touch_int.set_output_enable(false);
        self.touch_int.set_input_enable(true);
        self.touch_int.apply_input_config(&InputConfig::default());
        busy_delay(5_000_000);
        Ok(())
    }

    fn touch_pressed(&self) -> bool {
        self.touch_int.is_low()
    }

    fn touch_read_u16(&mut self, reg: u16) -> crate::Result<u16> {
        let mut value = [0u8; 2];
        self.read_register16(self.touch_addr, reg, &mut value)?;
        Ok(u16::from_le_bytes(value))
    }

    fn touch_set_interrupt_mode_low_level_query(&mut self) -> crate::Result<()> {
        let mut value = [0u8; 1];
        self.read_register16(self.touch_addr, GT911_MODULE_SWITCH_1, &mut value)?;
        value[0] = (value[0] & 0xFC) | 0x02;
        self.write_register16(self.touch_addr, GT911_MODULE_SWITCH_1, &value)?;
        self.touch_reload_config()
    }

    fn touch_reload_config(&mut self) -> crate::Result<()> {
        let mut config = [0u8; GT911_CONFIG_LENGTH - 2];
        self.read_register16(self.touch_addr, GT911_CONFIG_VERSION, &mut config)?;
        let checksum = (!config.iter().fold(0u8, |sum, value| sum.wrapping_add(*value))).wrapping_add(1);
        self.write_register16(self.touch_addr, GT911_CONFIG_CHKSUM, &[checksum])?;
        self.write_register16(self.touch_addr, GT911_CONFIG_FRESH, &[0x01])?;
        Ok(())
    }

    fn input_state(&mut self) -> crate::Result<InputState> {
        self.ensure_touch()?;

        if !self.touch_pressed() {
            return Ok(InputState::default());
        }

        let mut point_info = [0u8; 1];
        self.read_register16(self.touch_addr, GT911_POINT_INFO, &mut point_info)?;
        let status = point_info[0];
        let home = status & 0x10 != 0;
        let count = status & 0x0F;
        let buffer_ready = status & 0x80 != 0;
        if !buffer_ready && count == 0 {
            return Ok(InputState::default());
        }
        debug!("touch state: point_info=0x{:02X}", status);
        self.write_register16(self.touch_addr, GT911_POINT_INFO, &[0x00])?;

        let mut input = InputState {
            buttons: Buttons {
                home,
                auxiliary: self.auxiliary_button_pressed()?,
                boot: false,
            },
            ..InputState::default()
        };

        if count == 0 {
            return Ok(input);
        }

        let read_count = count.min(5) as usize;
        let mut buffer = [0u8; 39];
        self.read_register16(self.touch_addr, GT911_POINT_1, &mut buffer)?;

        let mut state = TouchState {
            count: read_count as u8,
            ..TouchState::default()
        };

        for i in 0..read_count {
            let offset = i * 8;
            state.points[i].id = buffer[offset];
            let raw_x = u16::from_le_bytes([buffer[offset + 1], buffer[offset + 2]]);
            let raw_y = u16::from_le_bytes([buffer[offset + 3], buffer[offset + 4]]);
            let (x, y) = if self.touch_resolution == (540, 960) {
                let x = (u32::from(raw_y)
                    * u32::from(crate::display::Display::WIDTH - 1)
                    / u32::from(self.touch_resolution.1 - 1)) as u16;
                let y = (u32::from(self.touch_resolution.0.saturating_sub(1).saturating_sub(raw_x))
                    * u32::from(crate::display::Display::HEIGHT - 1)
                    / u32::from(self.touch_resolution.0 - 1)) as u16;
                (x, y)
            } else if self.touch_resolution.0 > 1 && self.touch_resolution.1 > 1 {
                let x = (u32::from(raw_x) * u32::from(crate::display::Display::WIDTH - 1)
                    / u32::from(self.touch_resolution.0 - 1)) as u16;
                let y = (u32::from(raw_y) * u32::from(crate::display::Display::HEIGHT - 1)
                    / u32::from(self.touch_resolution.1 - 1)) as u16;
                (x, y)
            } else {
                (raw_x, raw_y)
            };
            debug!(
                "touch point raw=({}, {}) mapped=({}, {})",
                raw_x, raw_y, x, y
            );
            state.points[i].x = x;
            state.points[i].y = y;
            state.points[i].size = u16::from_le_bytes([buffer[offset + 5], buffer[offset + 6]]);
        }

        input.touch = Some(state);

        Ok(input)
    }

    fn touch_resolution(&self) -> (u16, u16) {
        self.touch_resolution
    }
}

pub struct PinConfig<'a> {
    pub data0: peripherals::GPIO5<'a>,
    pub data1: peripherals::GPIO6<'a>,
    pub data2: peripherals::GPIO7<'a>,
    pub data3: peripherals::GPIO15<'a>,
    pub data4: peripherals::GPIO16<'a>,
    pub data5: peripherals::GPIO17<'a>,
    pub data6: peripherals::GPIO18<'a>,
    pub data7: peripherals::GPIO8<'a>,
    pub i2c_sda: peripherals::GPIO39<'a>,
    pub i2c_scl: peripherals::GPIO40<'a>,
    pub leh: peripherals::GPIO42<'a>,
    pub lcd_dc: peripherals::GPIO41<'a>,
    pub lcd_wrx: peripherals::GPIO4<'a>,
    pub rmt: peripherals::GPIO48<'a>,
    pub stv: peripherals::GPIO45<'a>,
    pub touch_int: peripherals::GPIO3<'a>,
    pub touch_rst: peripherals::GPIO9<'a>,
    pub boot_btn: peripherals::GPIO0<'a>,
}

pub(crate) struct ED047TC1<'a> {
    i8080: Option<i8080::I8080<'a, Blocking>>,
    cfg_writer: ConfigWriter<'a>,
    rmt: rmt::Rmt<'a>,
    dma_buf: Option<DmaTxBuf>,
    boot_btn: Input<'a>,
}

impl<'a> ED047TC1<'a> {
    pub(crate) fn new(
        pins: PinConfig<'a>,
        i2c: peripherals::I2C0<'a>,
        dma: peripherals::DMA_CH0<'a>,
        lcd_cam: peripherals::LCD_CAM<'a>,
        rmt: peripherals::RMT<'a>,
    ) -> crate::Result<Self> {
        let lcd_cam = LcdCam::new(lcd_cam);

        let mut cfg_writer =
            ConfigWriter::new(
                i2c,
                pins.i2c_sda,
                pins.i2c_scl,
                pins.leh,
                pins.stv,
                pins.touch_rst,
                pins.touch_int,
            )?;
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
                    .expect("to create i8080 device")
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
            boot_btn: Input::new(pins.boot_btn, InputConfig::default().with_pull(Pull::Up)),
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
        while !self.cfg_writer.pwrgood()? {}
        self.cfg_writer.enable_tps()?;
        let mut tries = 0;
        while !self.cfg_writer.tps_power_good()? {
            tries += 1;
            if tries >= 500 {
                return Err(crate::Error::PowerTimeout);
            }
            busy_delay(240_000);
        }
        let _ = self.cfg_writer.ensure_touch();
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

    pub(crate) fn input_state(&mut self) -> crate::Result<InputState> {
        let mut input = self.cfg_writer.input_state()?;
        input.buttons.auxiliary = self.cfg_writer.auxiliary_button_pressed()?;
        input.buttons.boot = self.boot_btn.is_low();
        Ok(input)
    }

    pub(crate) fn touch_resolution(&self) -> (u16, u16) {
        self.cfg_writer.touch_resolution()
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

        let data = pulse!(output_time, 50);
        let rmt_tx = self.rmt.pulse(&data, false)?;
        let i8080 = self.i8080.take().ok_or(crate::Error::MissingI8080)?;
        let dma_buf = self.dma_buf.take().ok_or(crate::Error::MissingDmaBuffer)?;
        let tx = i8080
            .send(Command::<u8>::One(0), 0, dma_buf)
            .map_err(|(err, i8080, buf)| {
                self.dma_buf = Some(buf);
                self.i8080 = Some(i8080);
                crate::Error::Dma(err)
            })?;
        let (r, i8080, dma_buf) = tx.wait();
        if let Some(rmt_tx) = rmt_tx {
            self.rmt.reclaim_channel(rmt_tx)?;
        }
        r.map_err(crate::Error::Dma)?;
        self.i8080 = Some(i8080);
        self.dma_buf = Some(dma_buf);

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

fn parse_gt911_chip_id(product_id: [u8; 4]) -> u32 {
    let mut value = 0u32;
    for digit in product_id {
        if digit == 0 {
            break;
        }
        if !digit.is_ascii_digit() {
            return 0;
        }
        value = value * 10 + (digit - b'0') as u32;
    }
    value
}

#[inline(always)]
fn busy_delay(wait_cycles: u32) {
    let target = cycles() + wait_cycles as u64;
    while cycles() < target {}
}

#[inline(always)]
fn cycles() -> u64 {
    esp_hal::xtensa_lx::timer::get_cycle_count() as u64
}
