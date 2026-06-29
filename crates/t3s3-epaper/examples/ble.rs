//! BLE <-> LoRa bridge with an e-paper mirror.
//!
//! Advertises as "T3S3-Msg" and exposes a Nordic UART Service: RX
//! `6e400002-...` (write, central -> board, forwarded to LoRa) and TX
//! `6e400003-...` (notify, board -> central: BLE echo + LoRa receipts), under
//! service `6e400001-...`. A BLE write and a received LoRa packet are each
//! shown on the e-paper and relayed to the other side.
//!
//! Flash with `cargo run --release --example ble` (requires the `esp` toolchain
//! and espflash; `--release` because esp-radio's scheduling is
//! timing-sensitive). Drive it from a host with `tools/ble.py` (`--send`,
//! `--listen`, `--interact`).
//!
//! The BLE host runs on core 0 (HCI pump on its own task so the handshake stays
//! reliable); the blocking LoRa + ~2 s e-paper work runs on core 1, the two
//! linked by non-blocking `embassy-sync` channels. The scheduler tick is raised
//! to 1000 Hz (`.cargo/config.toml`) so the radio thread is serviced on time,
//! and the radio stays in continuous receive so it never duty-cycles off-air.

#![no_std]
#![no_main]
// the #[characteristic] macro expands to a borrow clippy flags; not our code.
#![allow(clippy::needless_borrows_for_generic_args)]

use core::fmt::Write as _;

use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::{Duration, Timer};
use embedded_graphics::{
    draw_target::DrawTarget,
    mono_font::{
        MonoTextStyle,
        ascii::{FONT_6X10, FONT_10X20},
    },
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{Line, PrimitiveStyle},
    text::Text,
};
use embedded_hal::delay::DelayNs as _;
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    delay::Delay,
    gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull},
    interrupt::software::SoftwareInterruptControl,
    spi::{
        Mode,
        master::{Config as SpiConfig, Spi},
    },
    system::Stack as CpuStack,
    time::Rate,
    timer::timg::TimerGroup,
};
use esp_println::println;
use esp_radio::ble::controller::BleConnector;
use t3s3_epaper::{
    ssd1680::{Display, Rotation},
    sx1262::{Config as RadioConfig, Sx1262},
};
use static_cell::StaticCell;
use trouble_host::prelude::*;

esp_bootloader_esp_idf::esp_app_desc!();

/// advertised name; this is what ble.py / a phone sees in a scan.
const DEVICE_NAME: &str = "T3S3-Msg";

/// max bytes per message in either direction.
const MSG_CAP: usize = 64;

const CONNECTIONS_MAX: usize = 1;
const L2CAP_CHANNELS_MAX: usize = 2; // signalling + att

/// stack for the core-1 (LoRa + display) thread; sized for the e-paper
/// framebuffer.
const CORE1_STACK: usize = 16 * 1024;

type Msg = heapless::Vec<u8, MSG_CAP>;

// concrete types so the controller pump can live in its own (non-generic)
// embassy task with a 'static lifetime.
type BleController = ExternalController<BleConnector<'static>, 20>;
type Resources = HostResources<DefaultPacketPool, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX>;

// cross-core mailboxes. CriticalSectionRawMutex is multi-core safe; both sides
// use the non-blocking try_* methods so neither core waits on the other.
static BLE_TO_LORA: Channel<CriticalSectionRawMutex, Msg, 4> = Channel::new();
static LORA_TO_BLE: Channel<CriticalSectionRawMutex, Msg, 4> = Channel::new();

// nordic uart service: a de-facto standard "serial over ble" layout that
// generic tools recognise. rx = central -> peripheral, tx = peripheral ->
// central.
#[gatt_server]
struct Server {
    nus: NusService,
}

#[gatt_service(uuid = "6e400001-b5a3-f393-e0a9-e50e24dcca9e")]
struct NusService {
    #[characteristic(uuid = "6e400002-b5a3-f393-e0a9-e50e24dcca9e", write, read)]
    rx: heapless::Vec<u8, MSG_CAP>,
    #[characteristic(uuid = "6e400003-b5a3-f393-e0a9-e50e24dcca9e", read, notify)]
    tx: heapless::Vec<u8, MSG_CAP>,
}

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    esp_println::logger::init_logger_from_env();
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));

    // esp-radio needs a heap; 72 KiB matches the trouble-host esp32 examples.
    esp_alloc::heap_allocator!(size: 72 * 1024);

    // the scheduler drives the radio's internal tasks and the embassy executor.
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    // core 1 owns the LoRa radio and the e-paper, both of which block (LoRa SPI,
    // and the ~2 s e-paper refresh). running them here keeps the BLE host on
    // core 0 responsive no matter how long a refresh takes.
    static CORE1: StaticCell<CpuStack<CORE1_STACK>> = StaticCell::new();
    let core1_stack = CORE1.init(CpuStack::new());
    esp_rtos::start_second_core(
        peripherals.CPU_CTRL,
        sw_int.software_interrupt1,
        core1_stack,
        move || {
            lora_display_loop(
                peripherals.SPI2,
                peripherals.GPIO5,
                peripherals.GPIO6,
                peripherals.GPIO3,
                peripherals.GPIO7,
                peripherals.GPIO8,
                peripherals.GPIO34,
                peripherals.GPIO33,
                peripherals.GPIO35,
                peripherals.SPI3,
                peripherals.GPIO14,
                peripherals.GPIO11,
                peripherals.GPIO15,
                peripherals.GPIO16,
                peripherals.GPIO47,
                peripherals.GPIO48,
            )
        },
    );

    let connector = BleConnector::new(peripherals.BT, Default::default()).unwrap();
    let controller: BleController = ExternalController::new(connector);

    // a fixed random address keeps the device recognisable across reboots.
    let address = Address::random([0x37, 0x53, 0x33, 0x42, 0x6c, 0xe5]);
    println!("ble: our address = {:?}", address.addr.raw());

    // the stack and its resources must be 'static so the controller pump can run
    // on its own task; park them in StaticCells (one instance, taken once).
    static RESOURCES: StaticCell<Resources> = StaticCell::new();
    static STACK: StaticCell<Stack<'static, BleController, DefaultPacketPool>> = StaticCell::new();
    let resources = RESOURCES.init(Resources::new());
    let stack: &'static Stack<'static, BleController, DefaultPacketPool> =
        STACK.init(trouble_host::new(controller, resources).set_random_address(address));
    let Host {
        mut peripheral,
        runner,
        ..
    } = stack.build();

    // pump the controller on its own task so the connection handshake's HCI
    // traffic is serviced promptly and never waits behind gatt/advertise work.
    // invariant: spawned exactly once, and the task pool has capacity for it.
    let token = ble_runner(runner).expect("ble runner task is spawned only once");
    spawner.spawn(token);

    let server = Server::new_with_config(GapConfig::Peripheral(PeripheralConfig {
        name: DEVICE_NAME,
        appearance: &appearance::power_device::GENERIC_POWER_DEVICE,
    }))
    .unwrap();

    println!("ble: advertising as \"{DEVICE_NAME}\", waiting for a central to connect");

    loop {
        match advertise(DEVICE_NAME, &mut peripheral, &server).await {
            Ok(conn) => gatt_events_task(&server, &conn).await,
            Err(e) => panic!("ble: advertise error: {e:?}"),
        }
    }
}

/// must run for the whole lifetime of the stack; it pumps the controller.
#[embassy_executor::task]
async fn ble_runner(mut runner: Runner<'static, BleController, DefaultPacketPool>) {
    loop {
        if let Err(e) = runner.run().await {
            panic!("ble: runner error: {e:?}");
        }
    }
}

/// handle GATT traffic until the central disconnects, while forwarding LoRa
/// receipts out as TX notifications.
async fn gatt_events_task<P: PacketPool>(server: &Server<'_>, conn: &GattConnection<'_, '_, P>) {
    let rx = &server.nus.rx;
    let tx = &server.nus.tx;
    loop {
        // race a gatt event against a short poll of the LoRa -> BLE mailbox so
        // received packets get notified out without blocking on either source.
        match select(conn.next(), Timer::after(Duration::from_millis(50))).await {
            Either::First(GattConnectionEvent::Disconnected { reason }) => {
                println!("ble: disconnected: {reason:?}");
                return;
            }
            Either::First(GattConnectionEvent::Gatt { event }) => {
                if let GattEvent::Write(write) = &event
                    && write.handle() == rx.handle
                {
                    let data = write.data();
                    match core::str::from_utf8(data) {
                        Ok(text) => {
                            println!("ble: received message: {text:?} ({} bytes)", data.len())
                        }
                        Err(_) => {
                            println!("ble: received {} bytes (non-utf8): {data:?}", data.len())
                        }
                    }
                    if let Ok(msg) = Msg::from_slice(data) {
                        // hand off to core 1 (display + LoRa tx); drop if full.
                        let _ = BLE_TO_LORA.try_send(msg.clone());
                        // echo back so the central sees an ack.
                        let _ = tx.notify(conn, &msg).await;
                    }
                }
                match event.accept() {
                    Ok(reply) => reply.send().await,
                    Err(e) => println!("ble: error accepting gatt event: {e:?}"),
                }
            }
            Either::First(_) => {}
            Either::Second(()) => {
                // poll tick: forward anything core 1 received over LoRa.
                while let Ok(msg) = LORA_TO_BLE.try_receive() {
                    println!("ble: notifying central of {} bytes from LoRa", msg.len());
                    let _ = tx.notify(conn, &msg).await;
                }
            }
        }
    }
}

/// advertise (connectable) and wait for a central to connect.
async fn advertise<'values, 'server, C: Controller>(
    name: &'values str,
    peripheral: &mut Peripheral<'values, C, DefaultPacketPool>,
    server: &'server Server<'values>,
) -> Result<GattConnection<'values, 'server, DefaultPacketPool>, BleHostError<C::Error>> {
    let mut adv_data = [0u8; 31];
    let len = AdStructure::encode_slice(
        &[
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            AdStructure::CompleteLocalName(name.as_bytes()),
        ],
        &mut adv_data[..],
    )?;
    let params = AdvertisementParameters {
        interval_min: Duration::from_millis(100),
        interval_max: Duration::from_millis(200),
        ..Default::default()
    };
    let advertiser = peripheral
        .advertise(
            &params,
            Advertisement::ConnectableScannableUndirected {
                adv_data: &adv_data[..len],
                scan_data: &[],
            },
        )
        .await?;
    let conn = advertiser.accept().await?.with_attribute_server(server)?;
    println!("ble: central connected");
    Ok(conn)
}

/// Core-1 loop: own the LoRa radio and the e-paper, bridge LoRa <-> BLE, and
/// mirror both directions to the display. All blocking work lives here.
#[allow(clippy::too_many_arguments)]
fn lora_display_loop(
    radio_spi: esp_hal::peripherals::SPI2<'static>,
    sck: esp_hal::peripherals::GPIO5<'static>,
    mosi: esp_hal::peripherals::GPIO6<'static>,
    miso: esp_hal::peripherals::GPIO3<'static>,
    nss: esp_hal::peripherals::GPIO7<'static>,
    rst: esp_hal::peripherals::GPIO8<'static>,
    busy: esp_hal::peripherals::GPIO34<'static>,
    dio1: esp_hal::peripherals::GPIO33<'static>,
    pow: esp_hal::peripherals::GPIO35<'static>,
    disp_spi: esp_hal::peripherals::SPI3<'static>,
    disp_sck: esp_hal::peripherals::GPIO14<'static>,
    disp_mosi: esp_hal::peripherals::GPIO11<'static>,
    disp_cs: esp_hal::peripherals::GPIO15<'static>,
    disp_dc: esp_hal::peripherals::GPIO16<'static>,
    disp_rst: esp_hal::peripherals::GPIO47<'static>,
    disp_busy: esp_hal::peripherals::GPIO48<'static>,
) -> ! {
    // lora radio on its own spi bus (sck=5, mosi=6, miso=3, nss=7).
    let spi = Spi::new(
        radio_spi,
        SpiConfig::default()
            .with_frequency(Rate::from_mhz(1))
            .with_mode(Mode::_0),
    )
    .unwrap()
    .with_sck(sck)
    .with_mosi(mosi)
    .with_miso(miso);
    let cs = Output::new(nss, Level::High, OutputConfig::default());
    let radio_rst = Output::new(rst, Level::High, OutputConfig::default());
    let radio_busy = Input::new(busy, InputConfig::default().with_pull(Pull::None));
    let radio_dio1 = Input::new(dio1, InputConfig::default().with_pull(Pull::None));
    // power the radio's oscillator rail; held high for the radio's lifetime.
    let _radio_pow = Output::new(pow, Level::High, OutputConfig::default());
    Delay::new().delay_ms(10);
    let mut radio = Sx1262::new(
        spi,
        cs,
        radio_rst,
        radio_busy,
        radio_dio1,
        Delay::new(),
        RadioConfig::default(),
    );
    radio.init().unwrap();
    println!("lora: ready at 915 MHz");

    // e-paper on a second spi bus (sclk=14, mosi=11, cs=15).
    let dspi = Spi::new(
        disp_spi,
        SpiConfig::default()
            .with_frequency(Rate::from_mhz(4))
            .with_mode(Mode::_0),
    )
    .unwrap()
    .with_sck(disp_sck)
    .with_mosi(disp_mosi);
    let dcs = Output::new(disp_cs, Level::High, OutputConfig::default());
    let dev = ExclusiveDevice::new(dspi, dcs, Delay::new()).unwrap();
    let dc = Output::new(disp_dc, Level::Low, OutputConfig::default());
    let drst = Output::new(disp_rst, Level::High, OutputConfig::default());
    let dbusy = Input::new(disp_busy, InputConfig::default().with_pull(Pull::None));
    let mut display = Display::new(dev, dc, drst, dbusy, Delay::new());
    display.set_rotation(Rotation::Rotate270); // landscape, 250 x 122

    display.init().unwrap();
    let mut last_ble = FmtBuf::new();
    let mut last_lora = FmtBuf::new();
    render(&mut display, last_ble.as_str(), last_lora.as_str());
    let _ = display.refresh();

    let mut rx_buf = [0u8; 255];
    let mut updates: u32 = 0;

    // continuous receive: the radio listens the whole time and holds a packet
    // (with dio1 high) until we read it, so we never duty-cycle the receiver. a
    // packet that lands during a slow display refresh is captured by the radio
    // and read on the next loop, rather than missed while we're off-air.
    radio.start_receive().unwrap();

    loop {
        let mut dirty = false;

        // service the radio first and every loop: a packet arrives
        // asynchronously and stays latched until read.
        match radio.try_receive(&mut rx_buf) {
            Ok(Some(info)) => {
                let payload = &rx_buf[..info.len];
                println!(
                    "lora: received {} bytes, rssi {} dBm, snr {} dB",
                    info.len, info.rssi_dbm, info.snr_db
                );
                if let Ok(msg) = Msg::from_slice(payload) {
                    let _ = LORA_TO_BLE.try_send(msg);
                }
                last_lora.clear();
                let _ = write!(last_lora, "LoRa<{}", show(payload));
                dirty = true;
            }
            Ok(None) => {}
            Err(e) => {
                // a crc error or transient fault: drop the packet and re-arm
                // rather than wedge the receiver in a half-configured state.
                println!("lora: rx error: {e:?}");
                let _ = radio.start_receive();
            }
        }

        // BLE -> LoRa: transmit anything queued. transmit() leaves the radio in
        // standby, so re-arm continuous receive once the burst is drained.
        let mut transmitted = false;
        while let Ok(msg) = BLE_TO_LORA.try_receive() {
            match radio.transmit(&msg) {
                Ok(()) => println!("lora: transmitted {} bytes", msg.len()),
                Err(e) => println!("lora: tx error: {e:?}"),
            }
            last_ble.clear();
            let _ = write!(last_ble, "BLE>{}", show(&msg));
            dirty = true;
            transmitted = true;
        }
        if transmitted {
            let _ = radio.start_receive();
        }

        if dirty {
            // lowest priority: the radio stays in continuous receive across the
            // refresh, so only a second packet within one refresh window is lost.
            render(&mut display, last_ble.as_str(), last_lora.as_str());
            updates += 1;
            // a full refresh now and then clears partial-refresh ghosting.
            if updates.is_multiple_of(10) {
                let _ = display.refresh();
            } else {
                let _ = display.refresh_partial();
            }
        } else {
            // nothing pending; back off briefly so we don't spin the spi bus hot.
            Delay::new().delay_ms(5);
        }
    }
}

/// render the bridge status: title, rule, and the last message each way.
fn render<D>(display: &mut D, ble_line: &str, lora_line: &str)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let title = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
    let body = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
    let rule = PrimitiveStyle::with_stroke(BinaryColor::On, 1);

    let _ = display.clear(BinaryColor::Off);
    let _ = Text::new("BLE <-> LoRa", Point::new(8, 24), title).draw(display);
    let _ = Line::new(Point::new(8, 32), Point::new(242, 32))
        .into_styled(rule)
        .draw(display);
    let _ = Text::new(ble_line, Point::new(8, 56), body).draw(display);
    let _ = Text::new(lora_line, Point::new(8, 72), body).draw(display);
}

/// a non-utf8-safe short preview of a payload for the display.
fn show(data: &[u8]) -> &str {
    core::str::from_utf8(data).unwrap_or("<binary>")
}

/// a fixed-capacity buffer implementing `core::fmt::Write` for allocator-free
/// string formatting.
struct FmtBuf {
    buf: [u8; 80],
    len: usize,
}

impl FmtBuf {
    fn new() -> Self {
        Self {
            buf: [0; 80],
            len: 0,
        }
    }

    fn clear(&mut self) {
        self.len = 0;
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl core::fmt::Write for FmtBuf {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let n = bytes.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&bytes[..n]);
        self.len += n;
        Ok(())
    }
}
