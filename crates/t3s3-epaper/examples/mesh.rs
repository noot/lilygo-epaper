//! nootmesh TDMA node: join (or found) the mesh, keep time via beacons, claim
//! a data slot, and show sync status on the e-paper display.
//!
//! Flash the same example onto multiple T3-S3 boards. With no GPS on this
//! board, the nodes elect a free-running root (lowest id wins) after a few
//! frames of listening; the others sync to its beacons. Watch the serial log
//! for rx/tx events and the display for root/stratum/slot.
//!
//! Flash with `cargo run --example mesh` (requires the `esp` toolchain +
//! espflash).

#![no_std]
#![no_main]

use core::{cell::RefCell, fmt::Write as _};

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
use embedded_hal_bus::spi::RefCellDevice;
use embedded_sdmmc::{Mode as SdMode, VolumeIdx};
use esp_backtrace as _;
use esp_hal::{
    delay::Delay,
    efuse::{self, InterfaceMacAddress},
    gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull},
    main,
    rng::Rng,
    spi::{
        Mode,
        master::{Config as SpiConfig, Spi},
    },
    time::{Instant, Rate},
};
use esp_println::println;
use nootmesh::{
    NodeId,
    airtime::Modulation,
    tdma::{Action, Engine},
    wire,
};
use t3s3_epaper::{
    ssd1680::{Display, Rotation},
    sx1262::{Bandwidth, CodingRate, Config, SpreadingFactor, Sx1262},
};

esp_bootloader_esp_idf::esp_app_desc!();

/// do a clean full refresh every this many partial refreshes to clear
/// ghosting.
const FULL_REFRESH_EVERY: u32 = 10;

/// the replay store's snapshot file, in the SD card's FAT root.
const STORE_FILE: &str = "STORE.BIN";

/// sized for the engine's full store (32 entries of framed 127-byte bodies);
/// static because it outsizes comfortable stack use.
const SNAP_BUF_LEN: usize = 6144;
static SNAP_BUF: static_cell::StaticCell<[u8; SNAP_BUF_LEN]> = static_cell::StaticCell::new();

/// fixed timestamp for FAT metadata: the board has no wall clock, and file
/// times play no protocol role.
struct FixedTime;

impl embedded_sdmmc::TimeSource for FixedTime {
    fn get_timestamp(&self) -> embedded_sdmmc::Timestamp {
        embedded_sdmmc::Timestamp {
            year_since_1970: 56,
            zero_indexed_month: 0,
            zero_indexed_day: 0,
            hours: 0,
            minutes: 0,
            seconds: 0,
        }
    }
}

/// read the persisted snapshot into `buf`, returning its length. every
/// failure stage logs its own error, so serial distinguishes a dead card
/// from a healthy card that simply has no snapshot yet.
fn load_snapshot<D, T>(
    volume_mgr: &mut embedded_sdmmc::VolumeManager<D, T>,
    buf: &mut [u8],
) -> Option<usize>
where
    D: embedded_sdmmc::BlockDevice,
    T: embedded_sdmmc::TimeSource,
{
    let volume = match volume_mgr.open_volume(VolumeIdx(0)) {
        Ok(volume) => volume,
        Err(e) => {
            println!("sd: open volume failed: {e:?}");
            return None;
        }
    };
    let root = match volume.open_root_dir() {
        Ok(root) => root,
        Err(e) => {
            println!("sd: open root dir failed: {e:?}");
            return None;
        }
    };
    let file = match root.open_file_in_dir(STORE_FILE, SdMode::ReadOnly) {
        Ok(file) => file,
        Err(embedded_sdmmc::Error::NotFound) => {
            println!("sd: card ok, no {STORE_FILE} yet (first boot)");
            return None;
        }
        Err(e) => {
            println!("sd: open {STORE_FILE} failed: {e:?}");
            return None;
        }
    };
    let mut n = 0;
    while !file.is_eof() && n < buf.len() {
        let read = match file.read(&mut buf[n..]) {
            Ok(read) => read,
            Err(e) => {
                println!("sd: read {STORE_FILE} failed: {e:?}");
                return None;
            }
        };
        if read == 0 {
            break;
        }
        n += read;
    }
    Some(n)
}

fn save_snapshot<D, T>(volume_mgr: &mut embedded_sdmmc::VolumeManager<D, T>, bytes: &[u8]) -> bool
where
    D: embedded_sdmmc::BlockDevice,
    T: embedded_sdmmc::TimeSource,
{
    let volume = match volume_mgr.open_volume(VolumeIdx(0)) {
        Ok(volume) => volume,
        Err(e) => {
            println!("sd: save: open volume failed: {e:?}");
            return false;
        }
    };
    let Ok(root) = volume.open_root_dir() else {
        return false;
    };
    let file = match root.open_file_in_dir(STORE_FILE, SdMode::ReadWriteCreateOrTruncate) {
        Ok(file) => file,
        Err(e) => {
            println!("sd: save: open {STORE_FILE} failed: {e:?}");
            return false;
        }
    };
    if let Err(e) = file.write(bytes) {
        println!("sd: save: write failed: {e:?}");
        return false;
    }
    true
}

/// microseconds since boot; the engine's monotonic clock.
fn now_us() -> u64 {
    Instant::now().duration_since_epoch().as_micros()
}

/// derive the radio configuration from the fleet profile, so the modulation
/// the airtime math assumes is the modulation the radio actually uses.
fn radio_config(modulation: &Modulation) -> Config {
    Config {
        spreading_factor: match modulation.spreading_factor() {
            8 => SpreadingFactor::Sf8,
            9 => SpreadingFactor::Sf9,
            10 => SpreadingFactor::Sf10,
            11 => SpreadingFactor::Sf11,
            12 => SpreadingFactor::Sf12,
            _ => SpreadingFactor::Sf7,
        },
        bandwidth: match modulation.bandwidth_hz() {
            250_000 => Bandwidth::Bw250kHz,
            500_000 => Bandwidth::Bw500kHz,
            _ => Bandwidth::Bw125kHz,
        },
        coding_rate: match modulation.coding_rate_denominator() {
            6 => CodingRate::Cr4_6,
            7 => CodingRate::Cr4_7,
            8 => CodingRate::Cr4_8,
            _ => CodingRate::Cr4_5,
        },
        preamble_len: modulation.preamble_symbols(),
        ..Config::default()
    }
}

#[main]
fn main() -> ! {
    // NOTE: do NOT force CpuClock::max() — at 240 MHz esp-hal's fixed SPI input
    // delay mis-samples MISO on these GPIO-matrix pins. The default clock works.
    let peripherals = esp_hal::init(esp_hal::Config::default());

    // lora radio on its own spi bus: sck=5, mosi=6, miso=3, nss=7.
    let radio_spi = Spi::new(
        peripherals.SPI2,
        SpiConfig::default()
            .with_frequency(Rate::from_mhz(1))
            .with_mode(Mode::_0),
    )
    .unwrap()
    .with_sck(peripherals.GPIO5)
    .with_mosi(peripherals.GPIO6)
    .with_miso(peripherals.GPIO3);
    let radio_cs = Output::new(peripherals.GPIO7, Level::High, OutputConfig::default());
    let radio_rst = Output::new(peripherals.GPIO8, Level::High, OutputConfig::default());
    let radio_busy = Input::new(
        peripherals.GPIO34,
        InputConfig::default().with_pull(Pull::None),
    );
    let radio_dio1 = Input::new(
        peripherals.GPIO33,
        InputConfig::default().with_pull(Pull::None),
    );
    // power the radio's oscillator rail (gpio35); see the rx example.
    let _radio_pow = Output::new(peripherals.GPIO35, Level::High, OutputConfig::default());
    Delay::new().delay_ms(10);
    let modulation = Modulation::default();
    let mut radio = Sx1262::new(
        radio_spi,
        radio_cs,
        radio_rst,
        radio_busy,
        radio_dio1,
        Delay::new(),
        radio_config(&modulation),
    );
    radio.init().unwrap();

    // e-paper display on a second spi bus: sclk=14, mosi=11, cs=15.
    let disp_spi = Spi::new(
        peripherals.SPI3,
        SpiConfig::default()
            // the microSD shares this bus (its cs is GPIO13, and it needs
            // MISO=GPIO2 which the write-only panel doesn't); SD initialization
            // caps the clock at 400 kHz, and the panel's multi-hundred-ms
            // refresh dwarfs the slower transfers
            .with_frequency(Rate::from_khz(400))
            .with_mode(Mode::_0),
    )
    .unwrap()
    .with_sck(peripherals.GPIO14)
    .with_mosi(peripherals.GPIO11)
    .with_miso(peripherals.GPIO2);
    let shared_spi = RefCell::new(disp_spi);
    // hold the MISO pad's internal pull-up: sd DAT0 must idle high, the slot
    // may lack an external pull-up, and `with_miso` configures the pad
    // floating. the held Input keeps the pad setting while the gpio matrix
    // keeps routing it to the spi peripheral.
    let _miso_pullup = Input::new(
        unsafe { esp_hal::peripherals::GPIO2::steal() },
        InputConfig::default().with_pull(Pull::Up),
    );
    let disp_cs = Output::new(peripherals.GPIO15, Level::High, OutputConfig::default());
    let disp_dev = RefCellDevice::new(&shared_spi, disp_cs, Delay::new()).unwrap();
    let disp_dc = Output::new(peripherals.GPIO16, Level::Low, OutputConfig::default());
    let disp_rst = Output::new(peripherals.GPIO47, Level::High, OutputConfig::default());
    let disp_busy = Input::new(
        peripherals.GPIO48,
        InputConfig::default().with_pull(Pull::None),
    );
    let mut display = Display::new(disp_dev, disp_dc, disp_rst, disp_busy, Delay::new());
    display.set_rotation(Rotation::Rotate270); // landscape, 250 x 122

    // microSD on the shared bus: persists the replay store across reboots.
    // a missing/unreadable card degrades to the ram-only mailbox.
    let sd_cs = Output::new(peripherals.GPIO13, Level::High, OutputConfig::default());
    let sd_dev = RefCellDevice::new(&shared_spi, sd_cs, Delay::new()).unwrap();
    // sd wake: >=74 clocks with chip-select de-asserted, which a device
    // transaction cannot produce (it asserts cs) — write on the bus directly
    // while both chip-selects are parked high. without this the card never
    // enters spi mode and every command reports CardNotFound. (same pattern
    // as the t5s3 core's sd driver; 20 bytes = 160 clocks and a settle delay
    // give slow cards margin over the 74-clock minimum.)
    Delay::new().delay_ms(10);
    if let Err(e) = embedded_hal::spi::SpiBus::write(&mut *shared_spi.borrow_mut(), &[0xFF; 20]) {
        println!("sd: wake clocks failed: {e:?}");
    }
    let sdcard = embedded_sdmmc::SdCard::new(sd_dev, Delay::new());
    // probe the card before any FAT logic: a size readout proves SPI comms
    // and card acquisition, so later failures can only be filesystem-level
    match sdcard.num_bytes() {
        Ok(bytes) => println!("sd: card detected, {} MB", bytes / (1024 * 1024)),
        Err(e) => println!("sd: no card detected: {e:?}"),
    }
    let mut volume_mgr = embedded_sdmmc::VolumeManager::new(sdcard, FixedTime);

    // stable per-board identity from the efuse mac; trng entropy for the
    // engine's randomized skips and root-fallback jitter.
    let mac = efuse::interface_mac_address(InterfaceMacAddress::Station);
    let m = mac.as_bytes();
    let node_id = NodeId(u32::from_be_bytes([m[2], m[3], m[4], m[5]]));
    let rng = Rng::new();
    let seed = (u64::from(rng.random()) << 32) | u64::from(rng.random());

    let mut engine =
        Engine::new(nootmesh::tdma::Config::default(), modulation, node_id, seed).unwrap();
    // this board is always-on infrastructure: retain recent texts and answer
    // recap requests from nodes that were offline. the boot-time recap
    // rebuilds our own history from other store nodes after a power cycle.
    engine.enable_store();
    engine.request_recap();

    // reload the replay store persisted by the previous boot (remaining
    // retention was saved as durations, rebased onto this boot's clock)
    let snap_buf = SNAP_BUF.init([0; SNAP_BUF_LEN]);
    match load_snapshot(&mut volume_mgr, snap_buf) {
        Some(n) => {
            engine.store_restore(now_us(), &snap_buf[..n]);
            println!("sd: restored {} stored texts", engine.store_len());
        }
        None => println!("sd: no persisted store (missing card or first boot)"),
    }
    let mut saved_store_len = engine.store_len();

    println!(
        "nootmesh node {:08x} up (mac {mac}, status {:#04x}, device_errors {:#06x})",
        node_id.0,
        radio.status().unwrap(),
        radio.device_errors().unwrap()
    );

    display.init().unwrap();
    render_status(&mut display, node_id, &engine, now_us(), 0, 0, None, "");
    display.refresh().unwrap();

    radio.start_receive().unwrap();

    let mut delay = Delay::new();
    let mut buf = [0u8; 255];
    let mut rx_count: u32 = 0;
    let mut tx_count: u32 = 0;
    let mut refreshes: u32 = 0;
    let mut last_rssi_dbm: Option<i16> = None;
    let mut last_text = FmtBuf::new();

    loop {
        // persist the replay store whenever it changed (new text stored or an
        // entry aged out); the sd write briefly blocks the radio, the same
        // cost class as a display refresh
        if engine.store_len() != saved_store_len {
            saved_store_len = engine.store_len();
            match engine.store_snapshot(now_us(), &mut snap_buf[..]) {
                Some(bytes) => {
                    if !save_snapshot(&mut volume_mgr, bytes) {
                        println!("sd: store save failed");
                    }
                }
                None => println!("sd: snapshot buffer too small"),
            }
        }

        let action = engine.next_action(now_us());
        let deadline = match action {
            Action::Transmit { at_us } => at_us,
            Action::Listen { revisit_us } => revisit_us,
        };

        // receive until the deadline; any packet may reschedule, so re-plan.
        let mut got_packet = false;
        loop {
            // timestamp BEFORE the spi reads: dio1 latched at RxDone, at most
            // one poll period before this instant
            let t = now_us();
            if t >= deadline {
                break;
            }
            match radio.try_receive(&mut buf) {
                Ok(Some(info)) => {
                    got_packet = true;
                    rx_count = rx_count.wrapping_add(1);
                    last_rssi_dbm = Some(info.rssi_dbm);
                    match engine.on_packet(t, &buf[..info.len], info.rssi_dbm) {
                        Ok(received) => println!(
                            "rx {}B {received} rssi {} dBm snr {} dB",
                            info.len, info.rssi_dbm, info.snr_db
                        ),
                        Err(e) => println!("rx {}B undecodable: {e}", info.len),
                    }
                    break;
                }
                Ok(None) => {}
                Err(e) => println!("rx error: {e:?}"),
            }
            delay.delay_us(50);
        }
        if got_packet {
            // show chat texts from the mesh (this node has no keyboard, so it
            // only receives; the refresh briefly blocks the radio, which the
            // protocol tolerates)
            if let Some(incoming) = engine.take_text() {
                let text = core::str::from_utf8(&incoming.body).unwrap_or("<binary>");
                println!("text from {:08x}: {text}", incoming.from.0);
                last_text = FmtBuf::new();
                // claimed name plus id tail when known, bare id otherwise
                match engine
                    .alias_of(incoming.from)
                    .and_then(|bytes| core::str::from_utf8(bytes).ok())
                {
                    Some(name) => {
                        let _ = write!(
                            last_text,
                            "{name} ({:04x}): {text}",
                            incoming.from.0 & 0xffff
                        );
                    }
                    None => {
                        let _ = write!(last_text, "{:08x}: {text}", incoming.from.0);
                    }
                }
                render_status(
                    &mut display,
                    node_id,
                    &engine,
                    now_us(),
                    rx_count,
                    tx_count,
                    last_rssi_dbm,
                    last_text.as_str(),
                );
                let _ = display.refresh_partial();
            }
            continue;
        }

        let Action::Transmit { .. } = action else {
            continue;
        };
        let is_hello = match wire::decode(engine.packet()) {
            Ok(wire::Message::Beacon(b)) => {
                println!("tx beacon stratum {} frame {}", b.stratum, b.frame_number);
                false
            }
            Ok(wire::Message::Hello(h)) => {
                println!(
                    "tx hello slot {:?} ({} neighbors)",
                    h.slot,
                    h.neighbors.len()
                );
                true
            }
            Ok(wire::Message::Text(t)) => {
                println!(
                    "tx text {}B from {:08x} hops {}",
                    t.body.len(),
                    t.origin.0,
                    t.hops
                );
                true
            }
            Ok(wire::Message::Recap(_)) => {
                println!("tx recap request");
                true
            }
            Ok(wire::Message::Alias(a)) => {
                println!("tx alias from {:08x} hops {}", a.origin.0, a.hops);
                true
            }
            Err(_) => false,
        };
        match radio.transmit(engine.packet()) {
            Ok(()) => {
                engine.on_transmitted();
                tx_count = tx_count.wrapping_add(1);
            }
            Err(e) => println!("tx error: {e:?}"),
        }
        if let Err(e) = radio.start_receive() {
            println!("start_receive error: {e:?}");
        }

        // refresh the display right after our data-slot hello, but only every
        // 4th frame: the ~half-second refresh deafens the radio, and doing it
        // every frame would shadow the same slots after ours every single
        // frame — any neighbor whose slot fell there (or whose one-shot
        // request did) would be systematically unheard
        let refresh_frame = engine
            .position(now_us())
            .is_some_and(|p| p.frame_number.is_multiple_of(4));
        if is_hello && refresh_frame {
            render_status(
                &mut display,
                node_id,
                &engine,
                now_us(),
                rx_count,
                tx_count,
                last_rssi_dbm,
                last_text.as_str(),
            );
            refreshes = refreshes.wrapping_add(1);
            if refreshes.is_multiple_of(FULL_REFRESH_EVERY) {
                let _ = display.refresh();
            } else {
                let _ = display.refresh_partial();
            }
        }
    }
}

/// draw the node's sync status into the framebuffer.
#[allow(clippy::too_many_arguments)]
fn render_status<D>(
    display: &mut D,
    node_id: NodeId,
    engine: &Engine,
    now_us: u64,
    rx_count: u32,
    tx_count: u32,
    last_rssi_dbm: Option<i16>,
    last_text: &str,
) where
    D: DrawTarget<Color = BinaryColor>,
{
    let title_style = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
    let body = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
    let rule = PrimitiveStyle::with_stroke(BinaryColor::On, 1);

    let mut line1 = FmtBuf::new();
    match engine.slot() {
        Some(slot) => {
            let _ = write!(line1, "id {:08x}  slot {slot}", node_id.0);
        }
        None => {
            let _ = write!(line1, "id {:08x}  slot --", node_id.0);
        }
    }
    if let Some(position) = engine.position(now_us) {
        let _ = write!(line1, "  f {}", position.frame_number);
    }
    let mut line2 = FmtBuf::new();
    match engine.root(now_us) {
        Some((root, 0)) if root == node_id => {
            let _ = write!(line2, "ROOT (free-running)");
        }
        Some((root, stratum)) => {
            let _ = write!(line2, "root {:08x}  s{stratum}", root.0);
        }
        None => {
            let _ = write!(line2, "syncing...");
        }
    }
    let _ = write!(line2, "  peers {}", engine.peer_count(now_us));
    if let Some(dbm) = last_rssi_dbm {
        let _ = write!(line2, " {dbm}dBm");
    }
    let mut line3 = FmtBuf::new();
    let _ = write!(
        line3,
        "rx {rx_count}  tx {tx_count}  store {}",
        engine.store_len()
    );

    let _ = display.clear(BinaryColor::Off);
    let _ = Text::new("nootmesh", Point::new(8, 24), title_style).draw(display);
    let _ = Line::new(Point::new(8, 32), Point::new(242, 32))
        .into_styled(rule)
        .draw(display);
    let _ = Text::new(line1.as_str(), Point::new(8, 52), body).draw(display);
    let _ = Text::new(line2.as_str(), Point::new(8, 68), body).draw(display);
    let _ = Text::new(line3.as_str(), Point::new(8, 84), body).draw(display);
    let _ = Text::new(last_text, Point::new(8, 100), body).draw(display);
}

/// a tiny fixed-capacity buffer that implements `core::fmt::Write` so `write!`
/// can format strings without an allocator.
struct FmtBuf {
    buf: [u8; 48],
    len: usize,
}

impl FmtBuf {
    fn new() -> Self {
        Self {
            buf: [0; 48],
            len: 0,
        }
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
