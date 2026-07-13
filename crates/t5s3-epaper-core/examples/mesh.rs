//! nootmesh TDMA node with GPS: anchors the mesh timeline to UTC and serves
//! as the elected root for GPS-less peers (such as T3-S3 nodes running their
//! own `mesh` example).
//!
//! Every whole UTC second parsed from the GPS feeds the sync engine; the node
//! roots itself once it holds a fix (a GPS-anchored root outranks any
//! free-running one). The radio must match the T3-S3 modulation, so this
//! overrides the T5S3 driver's SF10 default down to SF7.
//!
//! Flash with `cargo run -p t5s3-epaper-core --example mesh --features
//! lora,gps`.

#![no_std]
#![no_main]

extern crate alloc;
extern crate t5s3_epaper_core;

use core::format_args;

use embedded_graphics::prelude::*;
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use esp_backtrace as _;
use esp_hal::{
    delay::Delay,
    efuse::{self, InterfaceMacAddress},
    main,
    rng::Rng,
    time::Instant,
};
use esp_println::println;
use nootmesh::{
    airtime::Modulation,
    tdma::{Action, Engine},
    wire,
    NodeId,
};
use t5s3_epaper_core::{
    display::Rectangle,
    gps::Gps,
    gps_pin_config,
    lora::{Bandwidth, CodingRate, Config, Lora, SpreadingFactor},
    lora_pin_config,
    pin_config,
    Display,
    DrawMode,
};
use u8g2_fonts::FontRenderer;

static FONT: FontRenderer = FontRenderer::new::<u8g2_fonts::fonts::u8g2_font_spleen12x24_mr>();

esp_bootloader_esp_idf::esp_app_desc!();

/// microseconds since boot; the engine's monotonic clock.
fn now_us() -> u64 {
    Instant::now().duration_since_epoch().as_micros()
}

/// derive the radio configuration from the fleet profile, so the modulation
/// the airtime math assumes is the modulation the radio actually uses (this
/// driver's own default is SF10, which the T3-S3 nodes cannot demodulate).
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
            250_000 => Bandwidth::Bw250,
            500_000 => Bandwidth::Bw500,
            _ => Bandwidth::Bw125,
        },
        coding_rate: match modulation.coding_rate_denominator() {
            6 => CodingRate::Cr4_6,
            7 => CodingRate::Cr4_7,
            8 => CodingRate::Cr4_8,
            _ => CodingRate::Cr4_5,
        },
        preamble_length: modulation.preamble_symbols(),
        ..Config::default()
    }
}

/// fix types trustworthy enough to anchor the mesh timeline to UTC.
fn fix_is_valid(gps: &Gps) -> bool {
    use nmea::sentences::FixType;
    matches!(
        gps.fix_type(),
        Some(FixType::Gps | FixType::DGps | FixType::Pps | FixType::Rtk | FixType::FloatRtk)
    )
}

#[main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default();
    let config = config.with_cpu_clock(esp_hal::clock::CpuClock::_240MHz);
    let peripherals = esp_hal::init(config);

    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);

    let i2c_bus =
        t5s3_epaper_core::i2c::Bus::new(peripherals.I2C0, peripherals.GPIO39, peripherals.GPIO40)
            .expect("to build i2c bus");
    let mut display = Display::new(
        pin_config!(peripherals),
        &i2c_bus,
        peripherals.DMA_CH0,
        peripherals.LCD_CAM,
        peripherals.RMT,
    )
    .expect("to initialize display");

    let mut delay = Delay::new();

    display.power_on().expect("to power on display");
    // LoRa and GPS share the VCC3V3 rail enabled by Display::new(); the
    // official firmware waits 1.5 s after power-on before talking to the radio.
    delay.delay_millis(1500);
    display.clear().expect("to clear screen");

    let bus = t5s3_epaper_core::spi::Bus::new(
        peripherals.SPI2,
        peripherals.GPIO14,
        peripherals.GPIO13,
        peripherals.GPIO21,
        peripherals.GPIO12,
        peripherals.GPIO46,
    )
    .expect("to build spi bus");
    let modulation = Modulation::default();
    let mut radio = Lora::new(
        &bus,
        lora_pin_config!(peripherals),
        &radio_config(&modulation),
    )
    .expect("to initialize LoRa");

    let mut gps = Gps::detect(peripherals.UART1, gps_pin_config!(peripherals), &mut delay)
        .expect("to detect and initialize GPS");
    println!("gps module: {:?}", gps.module());

    // stable per-board identity from the efuse mac; trng entropy for the
    // engine's randomized skips and root-fallback jitter.
    let mac = efuse::interface_mac_address(InterfaceMacAddress::Station);
    let m = mac.as_bytes();
    let node_id = NodeId(u32::from_be_bytes([m[2], m[3], m[4], m[5]]));
    let rng = Rng::new();
    let seed = (u64::from(rng.random()) << 32) | u64::from(rng.random());

    let mut engine = Engine::new(nootmesh::tdma::Config::default(), modulation, node_id, seed)
        .expect("slot budget fits the modulation");
    // catch up on chat missed while powered off: store nodes replay history
    engine.request_recap();

    println!("nootmesh node {:08x} up (gps root candidate)", node_id.0);

    let text_area = Rectangle {
        x: 40,
        y: 60,
        width: 880,
        height: 400,
    };
    render_status(&mut display, node_id, &engine, &gps, now_us(), 0, 0);
    display
        .flush(DrawMode::BlackOnWhite)
        .expect("to flush display");

    radio.start_receive().expect("to start receive");

    let mut buf = [0u8; 255];
    let mut rx_count: u32 = 0;
    let mut tx_count: u32 = 0;
    let mut last_utc_fed: u64 = 0;

    loop {
        let action = engine.next_action(now_us());
        let deadline = match action {
            Action::Transmit { at_us } => at_us,
            Action::Listen { revisit_us } => revisit_us,
        };

        // receive (and drain the gps uart) until the deadline; any packet or
        // gps second may reschedule, so re-plan after either.
        let mut replan = false;
        loop {
            // timestamp BEFORE the spi reads: dio1 latched at RxDone, at most
            // one poll period before this instant
            let t = now_us();
            if t >= deadline {
                break;
            }
            match radio.poll_receive(&mut buf) {
                Ok(Some(n)) => {
                    replan = true;
                    rx_count = rx_count.wrapping_add(1);
                    match engine.on_packet(t, &buf[..n]) {
                        Ok(received) => println!(
                            "rx {}B {received} rssi {} dBm snr {} dB",
                            n,
                            radio.rssi(),
                            radio.snr()
                        ),
                        Err(e) => println!("rx {n}B undecodable: {e}"),
                    }
                    break;
                }
                Ok(None) => {}
                Err(e) => println!("rx error: {e}"),
            }

            if gps.update().unwrap_or(0) > 0 && fix_is_valid(&gps) {
                if let Some(utc) = gps.utc_seconds() {
                    if utc != last_utc_fed {
                        last_utc_fed = utc;
                        engine.on_gps_second(now_us(), utc);
                        replan = true;
                        break;
                    }
                }
            }

            delay.delay_micros(50);
        }
        if replan {
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
            Err(e) => println!("tx error: {e}"),
        }
        if let Err(e) = radio.start_receive() {
            println!("start_receive error: {e}");
        }

        // refresh the display once per frame, right after our data-slot hello:
        // the panel blocks the loop for the refresh, so keep it away from
        // moments we expect to receive
        if is_hello {
            render_status(
                &mut display,
                node_id,
                &engine,
                &gps,
                now_us(),
                rx_count,
                tx_count,
            );
            if let Err(e) = display.flush_partial_fast(text_area) {
                println!("display flush error: {e:?}");
            }
        }
    }
}

/// draw the node's sync status into the framebuffer.
fn render_status<D>(
    display: &mut D,
    node_id: NodeId,
    engine: &Engine,
    gps: &Gps,
    now_us: u64,
    rx_count: u32,
    tx_count: u32,
) where
    D: DrawTarget<Color = Gray4>,
{
    let fix = if fix_is_valid(gps) {
        "fix"
    } else if gps.satellites_in_view() > 0 {
        "tracking"
    } else {
        "no fix"
    };

    let _ = FONT.render_aligned(
        format_args!(
            "nootmesh {:08x}\n\ngps:  {} ({} sats)\nrole: {}\nslot: {}\nframe: {}\npeers: {}\nrx {}  tx {}",
            node_id.0,
            fix,
            gps.satellites_in_view(),
            match engine.root(now_us) {
                Some((root, _)) if root == node_id => "ROOT",
                Some(_) => "synced",
                None => "syncing...",
            },
            engine.slot().map_or(-1, i32::from),
            engine.position(now_us).map_or(0, |p| p.frame_number),
            engine.peer_count(now_us),
            rx_count,
            tx_count,
        ),
        Point::new(60, 100),
        u8g2_fonts::types::VerticalPosition::Baseline,
        u8g2_fonts::types::HorizontalAlignment::Left,
        u8g2_fonts::types::FontColor::WithBackground {
            fg: Gray4::BLACK,
            bg: Gray4::WHITE,
        },
        display,
    );
}
