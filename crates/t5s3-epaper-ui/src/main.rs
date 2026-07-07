#![no_std]
#![no_main]

extern crate alloc;
extern crate t5s3_epaper_core;

mod datetime;
mod fmt;
mod keyboard;
mod layout;
mod pages;
mod screen;
mod settings;
mod state;
mod tls;
mod widgets;
mod wifi;

use alloc::{collections::BTreeSet, format, string::String, vec::Vec};

use embassy_executor::Spawner;
use embassy_time::{with_timeout, Duration, Timer};
use embedded_graphics::{
    mono_font::{
        ascii::{FONT_6X10, FONT_9X15, FONT_9X18_BOLD},
        MonoTextStyle,
    },
    prelude::*,
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    delay::Delay,
    interrupt::software::SoftwareInterruptControl,
    timer::timg::TimerGroup,
};
use t5s3_epaper_core::{
    display::DisplayRotation,
    input_pin_config,
    lora::Lora,
    pin_config,
    sdcard::DirectoryEntry,
    Clock,
    Controller,
    Display,
    DrawMode,
    FrontLight,
    SdCard,
};
#[cfg(feature = "gps")]
use t5s3_epaper_core::{gps::Gps, gps_pin_config};

#[cfg(feature = "gps")]
use crate::pages::gps::{
    area_km2,
    current_fix,
    download_button_hit,
    draw_full_controls,
    draw_gps_data,
    draw_map,
    full_touch,
    fullscreen_button_hit,
    gps_data_native_rect,
    map_area_tiles,
    map_cache_filename,
    map_cache_path,
    map_cell,
    map_request_path,
    parse_map,
    refresh_map_panel,
    render_full_map,
    show_download_progress,
    FullAction,
    GpsFix,
    MapView,
    DOWNLOAD_RADIUS,
    FULL_ZOOM_MAX,
    FULL_ZOOM_MIN,
    GPS_REFRESH_TICKS,
    MAP_CACHE_DIR,
    MAP_MAX_BYTES,
    MARKER_MOVE_PX,
    MARKER_REFRESH_TICKS,
};
use crate::{
    datetime::{status_date, status_time, status_time_secs},
    keyboard::Key,
    layout::{touch_to_screen, SCREEN_W},
    pages::{
        environment,
        files::{
            display_row_count,
            draw_file_list,
            draw_files_footer,
            draw_files_screen,
            file_list_native_rect,
            files_footer_native_rect,
            files_scroll_down_hit,
            files_scroll_up_hit,
            is_bmp,
            list_hit,
            load_dir,
            parent_path,
            view_image,
            Row,
            VISIBLE_ROWS,
        },
        frontlight::{
            brightness_native_rect,
            draw_brightness_area,
            draw_frontlight_screen,
            minus_hit,
            plus_hit,
            BRIGHTNESS_STEP,
        },
        home::{draw_home, hit_test, ICONS},
        info::{
            draw_info_screen,
            draw_info_values,
            info_values_rect,
            read_info,
            INFO_REFRESH_TICKS,
        },
        library,
        lora::{
            draw_list,
            draw_lora_screen,
            draw_lora_status,
            draw_message,
            lora_status_native_rect,
            make_radio,
            message_box_native_rect,
            received_native_rect,
            sent_native_rect,
            LIST_MAX,
            MSG_MAX,
            RECV_Y,
            SENT_Y,
        },
        music,
        notes,
        reader::{draw as draw_reader, is_reader, load_document, tap_zone, ReaderDoc, Tap},
        settings::{self as settings_page, MenuHit},
        sleep::{
            draw_power_off_screen,
            draw_screensaver,
            draw_sleep_screen,
            power_off_hit,
            show_wallpaper,
            sleep_now_hit,
        },
        weather,
    },
    screen::Screen,
    settings::Settings,
    state::Remote,
    widgets::{
        back_button_hit,
        draw_back_button,
        draw_status_bar,
        draw_statusbar_battery,
        draw_statusbar_time,
        statusbar_battery_rect,
        statusbar_time_rect,
        BATTERY_REFRESH_TICKS,
    },
    wifi::{
        set_utc_time,
        Event as WifiEvent,
        ScanEntry,
        RESYNC_INTERVAL_SECS,
        RETRY_INTERVAL_SECS,
    },
};

esp_bootloader_esp_idf::esp_app_desc!();

// the real battery pack capacity in mAh, measured as the gauge's coulomb count
// at charge termination. the BQ27220's profile defaults to 3000 mAh, which
// made the percentage top out at ~54%; the boot block below (re)programs the
// gauge whenever its stored design capacity differs. adjust for a new pack.
const BATTERY_MAH: u16 = 1620;

// last visited screen, stored in RTC fast memory so it survives the reset that
// deep sleep performs. zeroed (Home) on first boot, then retained across sleep.
#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut LAST_SCREEN: u8 = 0;

// local unix time of the last successful NTP sync, also kept in RTC fast memory
// so "time since sync" on the info page survives deep sleep. zero until the
// first sync of this power cycle.
#[esp_hal::ram(unstable(rtc_fast, persistent))]
pub(crate) static mut LAST_SYNC_UNIX: u64 = 0;

// a "[hh:mm:ss] " local-time prefix for a lora log entry, honoring the 12/24h
// setting. empty before the first clock sync, when there is no wall-clock time.
fn lora_stamp(clock: &mut Clock, settings: &Settings) -> String {
    match status_time_secs(clock, settings.tz_offset_hours) {
        Some((h, m, s)) if settings.time_24h => format!("[{h:02}:{m:02}:{s:02}] "),
        Some((h, m, s)) => {
            let suffix = if h < 12 { "am" } else { "pm" };
            let h12 = match h % 12 {
                0 => 12,
                other => other,
            };
            format!("[{h12}:{m:02}:{s:02}{suffix}] ")
        }
        None => String::new(),
    }
}

// after a timezone or time-format change, repaint the status-bar clock so the
// shown time reflects the new setting immediately; returns the minute now shown
// so the once-a-minute tick stays in sync.
fn refresh_statusbar_clock(display: &mut Display, clock: &mut Clock, settings: &Settings) -> u32 {
    let now = status_time(clock, settings.tz_offset_hours);
    draw_statusbar_time(display, now, settings.time_24h);
    display.flush_partial_fast(statusbar_time_rect()).ok();
    now.map_or(60, |(_, m)| m)
}

// a wifi-task request running under the saved network credentials.
fn saved_request(settings: &Settings, op: wifi::Op) -> wifi::Request {
    wifi::Request {
        ssid: String::from(settings.wifi_ssid()),
        password: String::from(settings.wifi_password()),
        op,
    }
}

// the wifi-task operation the ui is waiting on, so its completion event can be
// routed back to the right page state. also the send gate: while this is Some,
// no new operation is queued (the task runs one at a time).
#[derive(Clone, Copy)]
enum Pending {
    Resync,
    // a user-initiated sync from the wifi page's Sync clock button; unlike
    // Resync it reports the outcome on that page's status line as an
    // internet-access check.
    SyncCheck,
    Scan,
    Join {
        reconnect: bool,
    },
    Music {
        inline: bool,
        command: Option<music::Button>,
    },
    Environment,
    Weather,
    #[cfg(feature = "gps")]
    MapTile {
        key: u32,
        dx: i32,
        dy: i32,
    },
    #[cfg(feature = "gps")]
    MapArea {
        total: usize,
        done: usize,
        km2: f32,
    },
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::_240MHz);
    let peripherals = esp_hal::init(config);

    // internal-RAM heaps for the wifi stack (its DMA buffers can't live in
    // PSRAM), plus a PSRAM heap for the display's large framebuffers. esp-hal
    // 1.1 dropped ESP_HAL_CONFIG_PSRAM_MODE, so request octal mode explicitly.
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);
    esp_alloc::heap_allocator!(size: 64 * 1024);
    esp_alloc::psram_allocator!(
        peripherals.PSRAM,
        esp_hal::psram,
        esp_hal::psram::PsramConfig {
            mode: esp_hal::psram::PsramMode::OctalSpi,
            ..Default::default()
        }
    );

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    // all wifi work runs on a dedicated task that owns the radio; the ui talks
    // to it over the request/event channels and stays responsive while
    // sessions run.
    match wifi::run() {
        Ok(token) => spawner.spawn(token),
        Err(e) => esp_println::println!("wifi: task spawn failed: {e:?}"),
    }

    // a cold boot needs a fresh time sync; a wake from deep sleep keeps the RTC.
    let woke = t5s3_epaper_core::power::wake_status().woke_from_deep_sleep();
    let mut clock = Clock::new(peripherals.LPWR);

    // user settings (timezone, time format, brightness, reader font size) read
    // from the NVS flash partition; falls back to build-time defaults on first
    // boot or if the stored blob is missing/invalid.
    let mut settings = Settings::load();

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
    // input (touch + all three buttons) runs on the shared i2c bus,
    // independent of the display driver.
    let mut input_ctl = Controller::new(&i2c_bus, input_pin_config!(peripherals));
    // battery-backed external rtc, also on the shared i2c bus: restored from
    // below whenever the internal clock was reset, written back on every
    // successful ntp sync.
    let rtc_ext = t5s3_epaper_core::pcf8563::Rtc::new(&i2c_bus);

    display.set_rotation(DisplayRotation::Rotate270);

    let mut light =
        FrontLight::new(peripherals.LEDC, peripherals.GPIO11).expect("to initialize front light");
    light.set_brightness(settings.brightness);

    let delay = Delay::new();
    display.power_on().expect("to power on");
    delay.delay_millis(10);
    display.clear().expect("to clear");

    // one SPI2 bus shared by the SD card and the LoRa radio, owned here and
    // lent out by reference so only a single owner ever exists. the bus also
    // owns both chip-selects, parked high whenever their device is idle, so
    // no page can ever see the other chip respond to its traffic.
    let bus = t5s3_epaper_core::spi::Bus::new(
        peripherals.SPI2,
        peripherals.GPIO14,
        peripherals.GPIO13,
        peripherals.GPIO21,
        peripherals.GPIO12,
        peripherals.GPIO46,
    )
    .expect("to build spi bus");

    // detect the GPS module BEFORE bringing up wifi. the L76K probe is a plain
    // UART exchange; doing it first keeps it in the same quiet, no-radio state
    // it was validated in (running it after the ~20s wifi sync was failing to
    // detect). the panel power-on + clear above gives the module time to boot.
    #[cfg(feature = "gps")]
    let mut gps: Option<Gps<'_>> = {
        let mut detect_delay = Delay::new();
        match Gps::detect(
            peripherals.UART1,
            gps_pin_config!(peripherals),
            &mut detect_delay,
        ) {
            Ok(g) => {
                esp_println::println!("detected GPS module: {:?}", g.module());
                Some(g)
            }
            Err(e) => {
                esp_println::println!("gps detection failed: {}", e);
                None
            }
        }
    };

    #[cfg(feature = "gps")]
    if let Some(ref mut g) = gps {
        for _ in 0..50 {
            g.update().ok();
            delay.delay_millis(20);
        }
    }

    // log the fuel gauge's capacity accounting once per boot: a full-charge
    // capacity far from the real pack shows up as a battery percentage that
    // tops out early, and a gauge stranded in config-update mode stops
    // gauging entirely (percentage frozen) — recover from the latter here.
    // this runs after the gps probe: the probe's timing is fragile (see the
    // comment above) and the capacity programming below can block for a few
    // seconds when it has to run.
    match display.fuel_gauge_diagnostics() {
        Ok(d) => {
            esp_println::println!("fuel gauge: {d:?}");
            if d.config_update {
                match display.fuel_gauge_exit_config_update() {
                    Ok(()) => esp_println::println!("fuel gauge: exited config-update mode"),
                    Err(e) => esp_println::println!("fuel gauge: config-update exit failed: {e}"),
                }
            }
            // the gauge's capacity profile is RAM-backed (reset to the 3000
            // mAh default only if the pack is ever unplugged), so re-program
            // the real pack capacity whenever the stored value differs.
            if d.design_mah != BATTERY_MAH {
                match display.fuel_gauge_program_capacity(BATTERY_MAH) {
                    Ok(()) => {
                        esp_println::println!("fuel gauge: programmed {BATTERY_MAH} mAh capacity")
                    }
                    Err(e) => esp_println::println!("fuel gauge: capacity programming failed: {e}"),
                }
            }
        }
        Err(e) => esp_println::println!("fuel gauge: diagnostics read failed: {e}"),
    }

    // whether the clock currently holds a real synced time: kept in the RTC
    // across a deep-sleep wake, otherwise only true once wifi/ntp succeeds. drives
    // the fast-retry cadence below until the first success.
    let mut clock_synced = woke;

    // any other reset (cold boot, reflash, crash) lost the internal clock, but
    // the battery-backed external rtc keeps ticking: restore from it, so wifi
    // is only needed when that too has lost time (first boot / battery pull).
    if !clock_synced {
        match rtc_ext.read_unix() {
            Ok(Some(unix)) => {
                clock.set_now_us(unix * 1_000_000);
                clock_synced = true;
                esp_println::println!("clock: restored from external rtc");
            }
            Ok(None) => esp_println::println!("clock: external rtc holds no valid time"),
            Err(e) => esp_println::println!("clock: external rtc read failed: {e}"),
        }
    }

    // sync the clock over wifi only when neither rtc had it (best effort,
    // with a timeout so it still boots when offline; skipped entirely when no
    // network is configured), then the radio powers down.
    if !clock_synced && !settings.wifi_ssid().is_empty() {
        Text::with_alignment(
            "syncing clock over wifi...",
            Point::new(SCREEN_W / 2, 400),
            MonoTextStyle::new(&FONT_9X15, Gray4::BLACK),
            Alignment::Center,
        )
        .draw(&mut display)
        .ok();
        display.flush(DrawMode::BlackOnWhite).expect("to flush");

        // the boot-time sync deliberately blocks until the session finishes
        // (the ui hasn't started yet); everything after boot goes through the
        // event router in the main loop instead.
        wifi::send(saved_request(&settings, wifi::Op::SyncTime));
        match wifi::next_event().await {
            WifiEvent::TimeSynced(Some(unix)) => {
                set_utc_time(&mut clock, unix);
                clock_synced = true;
                if let Err(e) = rtc_ext.set_unix(unix) {
                    esp_println::println!("clock: external rtc write failed: {e}");
                }
            }
            _ => esp_println::println!("clock: wifi/ntp sync failed, time unavailable"),
        }
    }

    // restore the screen we slept on (only after a real deep-sleep wake; any
    // other reset starts at Home). reading the RTC-backed static is sound as
    // the UI is single-threaded.
    let mut current_screen = if woke {
        Screen::from_index(unsafe { LAST_SCREEN })
    } else {
        Screen::Home
    };
    let mut needs_redraw = true;
    let mut brightness: u8 = settings.brightness;
    // whether a finger is currently down, so each tap is handled once on press.
    let mut touch_active = false;
    // whether the auxiliary button is currently held, so each press acts once.
    let mut aux_active = false;
    // set when Power Off is tapped on the sleep screen, to branch the teardown
    // below into a full PMIC shutdown instead of deep sleep.
    let mut power_off = false;
    // set when a settings value changes; the blob is written to flash once on
    // leaving the settings screen rather than on every tap, to spare flash wear.
    let mut settings_dirty = false;
    let mut last_status_minute: u32 = 60;
    // time of the last clock sync, used to schedule periodic re-syncs.
    let mut last_resync_secs = clock.now_us() / 1_000_000;
    // retry cadence while the clock is unsynced, doubled after each failed
    // attempt (up to the normal re-sync interval) so an unreachable network
    // doesn't freeze the ui with a blocking wifi session every two minutes.
    let mut resync_retry_secs = RETRY_INTERVAL_SECS;

    // lora send/receive state: the message being typed, a status line, the last
    // few sent and received messages, and the keyboard's symbol/shift toggles.
    // `radio` is live only while the lora screen is open (see the loop).
    let mut lora_message = String::new();
    let mut lora_status = String::from("type a message, then SEND");
    let mut lora_sent: Vec<String> = Vec::new();
    let mut lora_recv: Vec<String> = Vec::new();
    let mut radio: Option<Lora<'_, 'static>> = None;
    let mut radio_tried = false;
    let mut kb_symbols = false;
    let mut kb_shift = false;
    // ticks since the info page was last refreshed (uptime/temp/voltage).
    let mut info_refresh: u16 = 0;
    // ticks since the status-bar battery indicator was last refreshed.
    let mut battery_refresh: u16 = 0;

    // sd-card browser state: the directory being viewed, its (sorted) listing, a
    // scroll offset into that listing, a footer status/detail line, and a flag
    // that the listing needs (re)loading from the card on the next pass.
    let mut files_path = String::from("/");
    let mut files_entries: Vec<DirectoryEntry> = Vec::new();
    let mut files_scroll: usize = 0;
    let mut files_status = String::new();
    // set at declaration so a wake that restores the browser loads it.
    let mut files_dirty = current_screen == Screen::Files;
    // path of the .bmp currently shown full-screen by the image viewer.
    let mut image_path = String::new();

    // notes state: the /NOTES listing, a scroll offset into it, a footer status
    // line, and a flag that the listing needs (re)loading from the card on the
    // next pass (mirrors `files_dirty`; set at declaration so a wake that
    // restores the notes screen loads it). the editor holds the open note's
    // filename, its text, and a flag that the text needs reading from the card
    // on the next pass (mirrors `reader_dirty`).
    let mut notes_entries: Vec<notes::Entry> = Vec::new();
    let mut notes_scroll: usize = 0;
    let mut notes_status = String::new();
    let mut notes_dirty = current_screen == Screen::Notes;
    let mut note_name = String::new();
    let mut note_text = String::new();
    let mut note_dirty = false;
    // whether the editor's delete button is armed (two-tap confirm).
    let mut note_delete_armed = false;

    // reader state: the open text file, its paginated document (None if the
    // load failed), the current page, and a flag that the document needs
    // (re)loading from the card on the next pass (mirrors `files_dirty`).
    let mut reader_path = String::new();
    let mut reader_doc: Option<ReaderDoc> = None;
    let mut reader_dirty = false;
    // why the open failed, shown on the reader screen when `reader_doc` is None.
    let mut reader_status = String::new();

    // music / environment page state: the last fetched view plus its reload
    // flag (set on entry, on tap-to-refresh, and at declaration when a wake
    // restored the screen).
    let mut music = Remote::new(music::View::Loading, current_screen == Screen::Music);
    // a pending transport/volume control to POST on the next music fetch, or None
    // to just refresh the now-playing state.
    let mut music_command: Option<music::Button> = None;
    // whether the pending fetch is an in-page control press (keep the page up and
    // report ok/error on the bottom status line) rather than a full (re)load.
    let mut music_inline = false;
    // the bottom status line on the music page (control feedback), or None.
    let mut music_status: Option<&'static str> = None;
    // ticks since the "ok"/"error" status was shown, to auto-dismiss it.
    let mut music_status_ticks: u16 = 0;
    // ticks since the music progress line was last repainted.
    let mut music_refresh: u16 = 0;
    // anchor for extrapolating the playing track's position locally: (position at
    // last fetch, track duration, clock micros at that fetch). None unless a
    // track is actively playing with a known position.
    let mut music_anchor: Option<(u32, u32, u64)> = None;
    let mut env = Remote::new(
        environment::View::Loading,
        current_screen == Screen::Environment,
    );

    // weather page state: mirrors the environment page.
    let mut weather = Remote::new(weather::View::Loading, current_screen == Screen::Weather);

    // library (reader shelf) state: the scanned books plus their reload flag
    // (set on entry and on returning from the reader), and the scroll offset.
    let mut library = Remote::new(library::View::Loading, current_screen == Screen::Library);
    let mut library_scroll: usize = 0;

    // wifi settings page state: the last scan results, a status line, and flags
    // that a scan or a join should run on the next pass (mirrors env/weather).
    // `pw_mode` swaps the page to the password keyboard for the network being
    // joined (`pw_ssid`), accumulating the typed passphrase in `pw_buf`.
    let mut wifi_networks: Vec<ScanEntry> = Vec::new();
    let mut wifi_status = String::from("tap Scan to find networks");
    let mut wifi_scan_dirty = false;
    let mut wifi_join_dirty = false;
    // set when Sync clock is tapped: force an ntp sync over the saved network
    // on the next pass, doubling as an internet-access check.
    let mut wifi_sync_dirty = false;
    let mut wifi_pw_mode = false;
    let mut wifi_pw_ssid = String::new();
    let mut wifi_pw_buf = String::new();
    // set when the pending join is a reconnect to the already-saved network using
    // its stored password: on failure we fall back to the password keyboard so
    // stale credentials can be re-entered, rather than reporting a plain error.
    let mut wifi_reconnect = false;
    // the operation the wifi task is currently running for the ui, or None when
    // it is idle. gates sends (one operation at a time) and routes results.
    let mut wifi_pending: Option<Pending> = None;

    // the screen the reader returns to on Back: the file browser or the library,
    // depending on where the book was opened from.
    let mut reader_return = Screen::Files;

    #[cfg(feature = "gps")]
    let mut gps_refresh: u16 = 0;
    // last good position, kept so a dropped fix shows the previous coordinates
    // (marked stale) instead of blanking out.
    #[cfg(feature = "gps")]
    let mut last_fix: Option<GpsFix> = None;

    // the gps page's map panel: the last fetched view plus its reload flag (set
    // on entry, on tap-to-refresh, on tile crossings, and at declaration when a
    // wake restored the screen).
    #[cfg(feature = "gps")]
    let mut gps_map = Remote::new(MapView::Loading, current_screen == Screen::Gps);
    // set when the "save area" button is tapped: pre-download the cells around
    // the current fix into the sd cache on the next pass.
    #[cfg(feature = "gps")]
    let mut gps_download = false;
    // the map cell currently drawn and the marker offset last drawn for it, used
    // to follow the fix: reload on a tile crossing, nudge the marker within a
    // tile. None until the first map is drawn.
    #[cfg(feature = "gps")]
    let mut gps_shown_cell: Option<u32> = None;
    #[cfg(feature = "gps")]
    let mut gps_shown_offset = (0i32, 0i32);
    #[cfg(feature = "gps")]
    let mut gps_marker_ticks: u16 = 0;
    // digital zoom step for the fullscreen map view (0 = native zoom-15).
    #[cfg(feature = "gps")]
    let mut full_zoom: i32 = 0;

    loop {
        // route completed wifi-task work back into page state before drawing.
        // drained every pass; the task runs at most one operation at a time, so
        // `wifi_pending` says which page a result belongs to. partial repaints
        // only happen when the user is still on the page the result is for.
        while let Some(event) = wifi::poll_event() {
            let pending = wifi_pending.take();
            match event {
                WifiEvent::TimeSynced(result) => {
                    let sync_check = matches!(pending, Some(Pending::SyncCheck));
                    match result {
                        Some(unix) => {
                            set_utc_time(&mut clock, unix);
                            clock_synced = true;
                            needs_redraw = true;
                            resync_retry_secs = RETRY_INTERVAL_SECS;
                            if let Err(e) = rtc_ext.set_unix(unix) {
                                esp_println::println!("clock: external rtc write failed: {e}");
                            }
                            if sync_check {
                                wifi_status = String::from("internet ok - clock synced");
                            }
                        }
                        None => {
                            resync_retry_secs = (resync_retry_secs * 2).min(RESYNC_INTERVAL_SECS);
                            if sync_check {
                                wifi_status = String::from("check failed - no internet?");
                            }
                        }
                    }
                    last_resync_secs = clock.now_us() / 1_000_000;
                    // repaint the wifi page so the check's outcome shows even on
                    // failure (the success path already sets needs_redraw).
                    if sync_check && current_screen == Screen::SettingsWifi {
                        needs_redraw = true;
                    }
                }
                WifiEvent::ScanDone(result) => {
                    match result {
                        Some(nets) => {
                            wifi_status = if nets.is_empty() {
                                String::from("no networks found")
                            } else {
                                format!("{} networks - tap one to join", nets.len())
                            };
                            wifi_networks = nets;
                        }
                        None => wifi_status = String::from("scan failed"),
                    }
                    if current_screen == Screen::SettingsWifi {
                        needs_redraw = true;
                    }
                }
                WifiEvent::JoinDone(ok) => {
                    let reconnect = matches!(pending, Some(Pending::Join { reconnect: true }));
                    if ok {
                        settings.set_wifi(&wifi_pw_ssid, &wifi_pw_buf);
                        settings_dirty = true;
                        // a working network was just proven, so retry the clock
                        // sync on the normal cadence again.
                        resync_retry_secs = RETRY_INTERVAL_SECS;
                        // the new network may or may not be the one noot-server
                        // lives on: probe again on the next fetch.
                        wifi::reset_server_path();
                        wifi_status = format!("connected: {wifi_pw_ssid}");
                    } else if !wifi_pw_buf.is_empty() {
                        // the password may be wrong (stale for a reconnect, a
                        // typo for a fresh join): drop into the keyboard
                        // pre-filled with it so it can be corrected rather than
                        // retyped.
                        kb_symbols = false;
                        kb_shift = false;
                        wifi_pw_mode = true;
                        wifi_status = if reconnect {
                            String::from("reconnect failed - re-enter password")
                        } else {
                            String::from("join failed - check password")
                        };
                    } else {
                        wifi_status = String::from("join failed");
                    }
                    if current_screen == Screen::SettingsWifi {
                        needs_redraw = true;
                    }
                }
                WifiEvent::MusicDone(snapshot) => {
                    let (inline, command) = match pending {
                        Some(Pending::Music { inline, command }) => (inline, command),
                        _ => (false, None),
                    };
                    let on_music = current_screen == Screen::Music;
                    match snapshot {
                        Some(snap) => {
                            music.view = music::build_view(&snap.json, snap.cover.as_deref());
                            // anchor the local position tick to this fetch.
                            music_anchor = music::playback(&music.view)
                                .map(|p| (p.base_secs, p.duration_secs, clock.now_us()));
                            music_refresh = 0;
                            music_status_ticks = 0;
                            if !inline || command.is_some_and(music::Button::changes_art) {
                                // a (re)load, or a track change (next/previous),
                                // redraws fully so the album art re-renders.
                                // draw_screen shows the "ok" status on the
                                // repainted page when inline.
                                music_status = if inline { Some("ok") } else { None };
                                if on_music {
                                    needs_redraw = true;
                                }
                            } else if command.is_some_and(music::Button::changes_display) {
                                // play/pause: repaint just the band below the art.
                                music_status = Some("ok");
                                if on_music {
                                    music::redraw_body(&mut display, &music.view, Some("ok"));
                                    display.flush_partial_fast(music::below_art_rect()).ok();
                                }
                            } else {
                                // volume: only the status line needs updating.
                                music_status = Some("ok");
                                if on_music {
                                    music::draw_status(&mut display, "ok");
                                    display.flush_partial_fast(music::status_rect()).ok();
                                }
                            }
                        }
                        None if inline => {
                            // leave the page as-is; just report the failure.
                            music_status = Some("error");
                            music_status_ticks = 0;
                            if on_music {
                                music::draw_status(&mut display, "error");
                                display.flush_partial_fast(music::status_rect()).ok();
                            }
                        }
                        None => {
                            music_status = None;
                            music.view = music::View::Error;
                            if on_music {
                                needs_redraw = true;
                            }
                        }
                    }
                }
                WifiEvent::GotBody(body) => match pending {
                    Some(Pending::Environment) => {
                        env.view = match body {
                            Some(body) => environment::parse(&body),
                            None => environment::View::Error,
                        };
                        if current_screen == Screen::Environment {
                            needs_redraw = true;
                        }
                    }
                    Some(Pending::Weather) => {
                        weather.view = match body {
                            Some(body) => weather::parse(&body),
                            None => weather::View::Error,
                        };
                        if current_screen == Screen::Weather {
                            needs_redraw = true;
                        }
                    }
                    #[cfg(feature = "gps")]
                    Some(Pending::MapTile { key, dx, dy }) => {
                        gps_map.view = match body {
                            Some(body) => {
                                let view = parse_map(&body, dx, dy);
                                // only cache a body that decoded, so a bad
                                // download can never poison the cache.
                                if matches!(view, MapView::Ready { .. }) {
                                    match SdCard::new(&bus) {
                                        Ok(c) => {
                                            c.create_dir_all(MAP_CACHE_DIR).ok();
                                            if let Err(e) =
                                                c.write_file(map_cache_path(key).as_str(), &body)
                                            {
                                                esp_println::println!(
                                                    "gps: cache map failed: {e:?}"
                                                );
                                            }
                                        }
                                        Err(e) => esp_println::println!(
                                            "gps: cache sd init failed: {e:?}"
                                        ),
                                    }
                                }
                                view
                            }
                            None => MapView::Error,
                        };
                        // record what the panel now shows so the follow logic
                        // nudges the marker / reloads on tile crossings.
                        gps_marker_ticks = 0;
                        gps_shown_cell = Some(key);
                        gps_shown_offset = (dx, dy);
                        if current_screen == Screen::Gps {
                            refresh_map_panel(&mut display, &gps_map.view);
                        }
                    }
                    _ => {}
                },
                #[cfg(feature = "gps")]
                WifiEvent::Tile { key, body } => {
                    // one tile of an in-flight bulk download: cache it and
                    // advance the progress line. non-terminal, so the pending
                    // op is put back with its count advanced.
                    if let Some(Pending::MapArea { total, done, km2 }) = pending {
                        match SdCard::new(&bus) {
                            Ok(c) => {
                                if let Err(e) = c.write_file(map_cache_path(key).as_str(), &body) {
                                    esp_println::println!(
                                        "gps: cache tile {key:08X} failed: {e:?}"
                                    );
                                }
                            }
                            Err(e) => {
                                esp_println::println!("gps: save-area sd init failed: {e:?}")
                            }
                        }
                        let done = done + 1;
                        // refresh the progress line ~20 times over the download.
                        let step = (total / 20).max(1);
                        if (done.is_multiple_of(step) || done == total)
                            && current_screen == Screen::Gps
                        {
                            show_download_progress(&mut display, done, total);
                        }
                        wifi_pending = Some(Pending::MapArea { total, done, km2 });
                    }
                }
                #[cfg(feature = "gps")]
                WifiEvent::DownloadDone { fetched } => {
                    if let Some(Pending::MapArea { total, km2, .. }) = pending {
                        esp_println::println!("gps: area download saved {fetched}/{total} tiles");
                        // report the area now cached around the fix, then reload
                        // the current cell (now cached) on the next pass.
                        gps_map.view = MapView::Saved {
                            km2,
                            new_tiles: fetched,
                        };
                        if current_screen == Screen::Gps {
                            refresh_map_panel(&mut display, &gps_map.view);
                            Timer::after_millis(1500).await;
                        }
                        gps_map.invalidate();
                    }
                }
                #[cfg(not(feature = "gps"))]
                WifiEvent::Tile { .. } | WifiEvent::DownloadDone { .. } => {}
            }
        }

        if needs_redraw {
            let pct = display.battery_percentage().unwrap_or(0);
            let now = status_time(&mut clock, settings.tz_offset_hours);

            display.clear().ok();
            // the image viewer and fullscreen map paint full-screen, so they
            // skip the status bar.
            if current_screen != Screen::Image && current_screen != Screen::MapFull {
                draw_status_bar(&mut display, pct, now, settings.time_24h);
            }
            match current_screen {
                Screen::Home => draw_home(
                    &mut display,
                    status_date(&mut clock, settings.tz_offset_hours),
                    settings.icon_style,
                    settings.icon_size,
                ),
                Screen::Gps => {
                    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
                    draw_back_button(&mut display);
                    Text::with_alignment(
                        "GPS",
                        Point::new(SCREEN_W / 2, 120),
                        bold,
                        Alignment::Center,
                    )
                    .draw(&mut display)
                    .ok();

                    #[cfg(feature = "gps")]
                    match &gps {
                        Some(g) => {
                            draw_gps_data(&mut display, g, last_fix);
                            draw_map(&mut display, &gps_map.view);
                        }
                        None => {
                            let small = MonoTextStyle::new(&FONT_6X10, Gray4::new(4));
                            Text::with_alignment(
                                "no module detected",
                                Point::new(SCREEN_W / 2, 400),
                                small,
                                Alignment::Center,
                            )
                            .draw(&mut display)
                            .ok();
                        }
                    }

                    #[cfg(not(feature = "gps"))]
                    {
                        let small = MonoTextStyle::new(&FONT_6X10, Gray4::new(4));
                        Text::with_alignment(
                            "compile with --features gps",
                            Point::new(SCREEN_W / 2, 400),
                            small,
                            Alignment::Center,
                        )
                        .draw(&mut display)
                        .ok();
                    }
                }
                Screen::Lora => draw_lora_screen(
                    &mut display,
                    &lora_message,
                    &lora_status,
                    &lora_sent,
                    &lora_recv,
                    kb_symbols,
                    kb_shift,
                ),
                Screen::Frontlight => draw_frontlight_screen(&mut display, brightness),
                Screen::Sleep => draw_sleep_screen(&mut display),
                Screen::Info => {
                    let info = read_info(&mut display, &mut clock);
                    draw_info_screen(&mut display, &info);
                }
                Screen::Files => draw_files_screen(
                    &mut display,
                    &files_path,
                    &files_entries,
                    files_scroll,
                    &files_status,
                ),
                Screen::Image => {
                    if !view_image(&bus, &mut display, &image_path) {
                        Text::with_alignment(
                            "cannot display image",
                            Point::new(SCREEN_W / 2, 400),
                            MonoTextStyle::new(&FONT_9X15, Gray4::BLACK),
                            Alignment::Center,
                        )
                        .draw(&mut display)
                        .ok();
                    }
                    draw_back_button(&mut display);
                }
                Screen::Reader => {
                    draw_back_button(&mut display);
                    match &reader_doc {
                        Some(doc) => draw_reader(&mut display, doc),
                        None => {
                            Text::with_alignment(
                                "cannot open file",
                                Point::new(SCREEN_W / 2, 380),
                                MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK),
                                Alignment::Center,
                            )
                            .draw(&mut display)
                            .ok();
                            Text::with_alignment(
                                &reader_status,
                                Point::new(SCREEN_W / 2, 420),
                                MonoTextStyle::new(&FONT_9X15, Gray4::new(4)),
                                Alignment::Center,
                            )
                            .draw(&mut display)
                            .ok();
                        }
                    }
                }
                Screen::Settings => settings_page::draw_menu(&mut display),
                Screen::SettingsSystem => settings_page::system::draw(&mut display, &settings),
                Screen::SettingsReader => settings_page::reader::draw(&mut display, &settings),
                Screen::SettingsWifi => {
                    if wifi_pw_mode {
                        settings_page::wifi::draw_password(
                            &mut display,
                            &wifi_pw_ssid,
                            &wifi_pw_buf,
                            &wifi_status,
                            kb_symbols,
                            kb_shift,
                        );
                    } else {
                        settings_page::wifi::draw_status(
                            &mut display,
                            &settings,
                            &wifi_status,
                            &wifi_networks,
                        );
                    }
                }
                Screen::Music => music::draw_screen(&mut display, &music.view, music_status),
                Screen::Notes => notes::draw_list_screen(
                    &mut display,
                    &notes_entries,
                    notes_scroll,
                    &notes_status,
                ),
                Screen::NoteEdit => notes::draw_edit_screen(
                    &mut display,
                    &note_name,
                    &note_text,
                    kb_symbols,
                    kb_shift,
                    note_delete_armed,
                ),
                Screen::Environment => environment::draw_screen(&mut display, &env.view),
                Screen::Weather => weather::draw_screen(&mut display, &weather.view),
                Screen::Library => {
                    library::draw_screen(&mut display, &library.view, library_scroll)
                }
                #[cfg(feature = "gps")]
                Screen::MapFull => {
                    // stitch the fullscreen map from cached tiles (offline) and
                    // draw the zoom/back controls over it.
                    let card = match SdCard::new(&bus) {
                        Ok(c) => Some(c),
                        Err(e) => {
                            esp_println::println!("gps: fullscreen sd init failed: {e:?}");
                            None
                        }
                    };
                    render_full_map(&mut display, card.as_ref(), last_fix, full_zoom);
                    draw_full_controls(&mut display, full_zoom);
                }
                #[cfg(not(feature = "gps"))]
                Screen::MapFull => {}
            }
            // a transient flush error shouldn't reboot the ui mid-session; log
            // it and carry on (the next redraw will try again).
            if let Err(e) = display.flush(DrawMode::BlackOnWhite) {
                esp_println::println!("display flush failed: {e}");
            }
            needs_redraw = false;
            last_status_minute = now.map_or(60, |(_, m)| m);
            info_refresh = 0;
            battery_refresh = 0;

            #[cfg(feature = "gps")]
            {
                gps_refresh = 0;
            }
        }

        // tick the status-bar clock and battery once a minute via fast partial
        // refreshes.
        if !needs_redraw {
            if let Some((h, m)) = status_time(&mut clock, settings.tz_offset_hours) {
                if m != last_status_minute {
                    last_status_minute = m;
                    draw_statusbar_time(&mut display, Some((h, m)), settings.time_24h);
                    display.flush_partial_fast(statusbar_time_rect()).ok();

                    let pct = display.battery_percentage().unwrap_or(0);
                    draw_statusbar_battery(&mut display, pct);
                    display.flush_partial_fast(statusbar_battery_rect()).ok();
                }
            }
        }

        // refresh the status-bar battery indicator periodically so the charge
        // stays current without switching pages. the image viewer and
        // fullscreen map hide the status bar, so skip it there.
        if !needs_redraw && current_screen != Screen::Image && current_screen != Screen::MapFull {
            battery_refresh += 1;
            if battery_refresh >= BATTERY_REFRESH_TICKS {
                battery_refresh = 0;
                let pct = display.battery_percentage().unwrap_or(0);
                draw_statusbar_battery(&mut display, pct);
                display.flush_partial_fast(statusbar_battery_rect()).ok();
            }
        }

        // refresh the info page values periodically so uptime/since-sync tick.
        if current_screen == Screen::Info && !needs_redraw {
            info_refresh += 1;
            if info_refresh >= INFO_REFRESH_TICKS {
                info_refresh = 0;
                let info = read_info(&mut display, &mut clock);
                draw_info_values(&mut display, &info);
                display.flush_partial_fast(info_values_rect()).ok();
            }
        }

        // advance the music page's song position locally (no wifi): extrapolate
        // from the last fetch and repaint just the progress line. when the track
        // should have ended, pull the next one over wifi instead.
        if current_screen == Screen::Music && !needs_redraw {
            music_refresh += 1;
            if music_refresh >= music::REFRESH_TICKS {
                music_refresh = 0;
                if let Some((base, duration, at_us)) = music_anchor {
                    let elapsed = clock.now_us().saturating_sub(at_us) / 1_000_000;
                    let current = base as u64 + elapsed;
                    if current >= duration as u64 {
                        music_command = None;
                        music_inline = false;
                        music_anchor = None;
                        music.invalidate();
                    } else {
                        music::draw_progress(&mut display, current as u32, duration);
                        display.flush_partial_fast(music::progress_rect()).ok();
                    }
                }
            }
        }

        // auto-dismiss the control feedback ("ok"/"error") a few seconds after it
        // is shown, clearing it with its own small partial refresh.
        if current_screen == Screen::Music && !needs_redraw && music_status.is_some() {
            music_status_ticks += 1;
            if music_status_ticks >= music::REFRESH_TICKS {
                music_status = None;
                music::draw_status(&mut display, "");
                display.flush_partial_fast(music::status_rect()).ok();
            }
        }

        // periodically bring wifi back up briefly to re-sync the clock, then it
        // powers down again. correct against RTC drift without leaving the radio
        // on to interfere with gps. (steal WIFI: the previous controller was
        // dropped, so the peripheral is free to re-init.)
        // keep the tls layer's wall-clock view fresh for certificate checks.
        let now_secs = clock.now_us() / 1_000_000;
        if clock_synced {
            tls::set_now_unix(now_secs);
        }

        let resync_interval = if clock_synced {
            RESYNC_INTERVAL_SECS
        } else {
            resync_retry_secs
        };
        if wifi_pending.is_none()
            && !settings.wifi_ssid().is_empty()
            && now_secs >= last_resync_secs + resync_interval
        {
            esp_println::println!("clock: periodic re-sync");
            if wifi::send(saved_request(&settings, wifi::Op::SyncTime)) {
                wifi_pending = Some(Pending::Resync);
                // pushed forward again when the sync completes; this stamp just
                // keeps the send from repeating while the session runs.
                last_resync_secs = clock.now_us() / 1_000_000;
            }
        }

        // poll touch/buttons every pass so input stays responsive. the GPS
        // work below is non-blocking, so it never stalls this poll. a transient
        // read error shouldn't reboot the ui, so log it and retry next pass.
        let input = match input_ctl.state() {
            Ok(input) => input,
            Err(e) => {
                esp_println::println!("input read failed: {e}");
                delay.delay_millis(50);
                continue;
            }
        };

        if input.buttons.home && current_screen != Screen::Home {
            if current_screen == Screen::Reader {
                if let Some(doc) = &reader_doc {
                    doc.save(&bus);
                }
            }
            if current_screen == Screen::NoteEdit {
                notes::save(&bus, &note_name, &note_text);
            }
            current_screen = Screen::Home;
            needs_redraw = true;
        }

        // the auxiliary button turns the page in the reader, and sleeps from any
        // other screen (the current screen is restored on wake). edge-detected so
        // holding it acts once.
        if input.buttons.auxiliary {
            if !aux_active {
                aux_active = true;
                if current_screen == Screen::Reader {
                    if let Some(doc) = &mut reader_doc {
                        if doc.next_page() {
                            needs_redraw = true;
                        }
                    }
                } else {
                    break;
                }
            }
        } else {
            aux_active = false;
        }

        // edge-detect touches: act only on the press (untouched -> touched) and
        // wait for release before accepting the next, so a tap held longer than
        // one poll doesn't register repeatedly (double letters).
        match input.touch.and_then(|s| s.first_point()) {
            Some(point) if !touch_active => {
                touch_active = true;
                let (sx, sy) = touch_to_screen(point.x, point.y);

                match current_screen {
                    Screen::Home => {
                        if let Some(idx) = hit_test(sx, sy) {
                            current_screen = ICONS[idx].screen;
                            // the file browser draws only after its listing is
                            // loaded (below), so it sets `files_dirty` instead of
                            // redrawing now with an empty list.
                            match current_screen {
                                Screen::Files => {
                                    files_path = String::from("/");
                                    files_dirty = true;
                                }
                                // the notes list draws only after its listing is
                                // loaded too.
                                Screen::Notes => {
                                    notes_dirty = true;
                                }
                                // the server pages paint a "loading" view now,
                                // then fetch over wifi on the next pass.
                                Screen::Music => {
                                    music.refresh(music::View::Loading);
                                    music_command = None;
                                    music_inline = false;
                                    music_status = None;
                                    music_anchor = None;
                                    needs_redraw = true;
                                }
                                Screen::Environment => {
                                    env.refresh(environment::View::Loading);
                                    needs_redraw = true;
                                }
                                Screen::Weather => {
                                    weather.refresh(weather::View::Loading);
                                    needs_redraw = true;
                                }
                                // the shelf paints a "scanning" view now, then
                                // scans the card on the next pass.
                                Screen::Library => {
                                    library.refresh(library::View::Loading);
                                    library_scroll = 0;
                                    needs_redraw = true;
                                }
                                // the gps page paints its map panel as "loading"
                                // now, then fetches over wifi on the next pass.
                                #[cfg(feature = "gps")]
                                Screen::Gps => {
                                    gps_map.refresh(MapView::Loading);
                                    needs_redraw = true;
                                }
                                _ => needs_redraw = true,
                            }
                        }
                    }
                    Screen::Frontlight => {
                        if back_button_hit(sx, sy) {
                            current_screen = Screen::Home;
                            needs_redraw = true;
                        } else if minus_hit(sx, sy) {
                            brightness = brightness.saturating_sub(BRIGHTNESS_STEP);
                            light.set_brightness(brightness);
                            draw_brightness_area(&mut display, brightness);
                            display.flush_partial_fast(brightness_native_rect()).ok();
                        } else if plus_hit(sx, sy) {
                            brightness = brightness.saturating_add(BRIGHTNESS_STEP).min(100);
                            light.set_brightness(brightness);
                            draw_brightness_area(&mut display, brightness);
                            display.flush_partial_fast(brightness_native_rect()).ok();
                        }
                    }
                    Screen::Sleep => {
                        if back_button_hit(sx, sy) {
                            current_screen = Screen::Home;
                            needs_redraw = true;
                        } else if sleep_now_hit(sx, sy) {
                            // leave the loop to draw the screensaver and enter
                            // deep sleep below
                            break;
                        } else if power_off_hit(sx, sy) {
                            // leave the loop to power the board off below
                            power_off = true;
                            break;
                        }
                    }
                    Screen::Lora => {
                        if back_button_hit(sx, sy) {
                            current_screen = Screen::Home;
                            needs_redraw = true;
                        } else if let Some(key) = keyboard::hit(sx, sy, kb_symbols, kb_shift) {
                            match key {
                                Key::Shift => {
                                    kb_shift = !kb_shift;
                                    keyboard::draw(&mut display, kb_symbols, kb_shift, "SEND");
                                    display.flush_partial_fast(keyboard::native_rect()).ok();
                                }
                                Key::Symbols => {
                                    kb_symbols = !kb_symbols;
                                    keyboard::draw(&mut display, kb_symbols, kb_shift, "SEND");
                                    display.flush_partial_fast(keyboard::native_rect()).ok();
                                }
                                Key::Enter => {
                                    if lora_message.is_empty() {
                                        lora_status = String::from("nothing to send");
                                    } else if let Some(r) = &mut radio {
                                        match r.transmit(lora_message.as_bytes()) {
                                            Ok(()) => {
                                                esp_println::println!("lora tx: {lora_message}");
                                                lora_status =
                                                    format!("sent {} bytes", lora_message.len());
                                                lora_sent.push(format!(
                                                    "{}{lora_message}",
                                                    lora_stamp(&mut clock, &settings)
                                                ));
                                                if lora_sent.len() > LIST_MAX {
                                                    lora_sent.remove(0);
                                                }
                                                lora_message.clear();
                                            }
                                            Err(e) => {
                                                esp_println::println!("lora tx error: {e}");
                                                lora_status = format!("tx error: {e}");
                                            }
                                        }
                                        // resume listening after transmitting.
                                        r.start_receive().ok();
                                        draw_message(&mut display, &lora_message);
                                        display.flush_partial_fast(message_box_native_rect()).ok();
                                        draw_list(&mut display, SENT_Y, "sent", &lora_sent);
                                        display.flush_partial_fast(sent_native_rect()).ok();
                                    } else {
                                        lora_status = String::from("radio not ready");
                                    }
                                    draw_lora_status(&mut display, &lora_status);
                                    display.flush_partial_fast(lora_status_native_rect()).ok();
                                }
                                other => {
                                    match other {
                                        Key::Char(c) if lora_message.len() < MSG_MAX => {
                                            lora_message.push(c)
                                        }
                                        Key::Space if lora_message.len() < MSG_MAX => {
                                            lora_message.push(' ')
                                        }
                                        Key::Backspace => {
                                            lora_message.pop();
                                        }
                                        _ => {}
                                    }
                                    draw_message(&mut display, &lora_message);
                                    display.flush_partial_fast(message_box_native_rect()).ok();
                                }
                            }
                        }
                    }
                    Screen::Files => {
                        if back_button_hit(sx, sy) {
                            current_screen = Screen::Home;
                            needs_redraw = true;
                        } else if files_scroll_up_hit(sx, sy) {
                            if files_scroll > 0 {
                                files_scroll = files_scroll.saturating_sub(VISIBLE_ROWS);
                                draw_file_list(
                                    &mut display,
                                    &files_path,
                                    &files_entries,
                                    files_scroll,
                                );
                                display.flush_partial_fast(file_list_native_rect()).ok();
                            }
                        } else if files_scroll_down_hit(sx, sy) {
                            let total = display_row_count(&files_path, files_entries.len());
                            if files_scroll + VISIBLE_ROWS < total {
                                files_scroll += VISIBLE_ROWS;
                                draw_file_list(
                                    &mut display,
                                    &files_path,
                                    &files_entries,
                                    files_scroll,
                                );
                                display.flush_partial_fast(file_list_native_rect()).ok();
                            }
                        } else if let Some(row) =
                            list_hit(sx, sy, &files_path, files_entries.len(), files_scroll)
                        {
                            match row {
                                Row::Parent => {
                                    files_path = parent_path(&files_path);
                                    files_dirty = true;
                                }
                                Row::Entry(i) => {
                                    if let Some(entry) = files_entries.get(i) {
                                        if entry.is_directory {
                                            files_path = entry.path.clone();
                                            files_dirty = true;
                                        } else if is_bmp(&entry.name) {
                                            image_path = entry.path.clone();
                                            current_screen = Screen::Image;
                                            needs_redraw = true;
                                        } else if is_reader(&entry.name) {
                                            reader_path = entry.path.clone();
                                            reader_dirty = true;
                                            reader_return = Screen::Files;
                                            current_screen = Screen::Reader;
                                        } else {
                                            files_status =
                                                format!("{} - {} bytes", entry.name, entry.size);
                                            draw_files_footer(&mut display, &files_status);
                                            display
                                                .flush_partial_fast(files_footer_native_rect())
                                                .ok();
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Screen::Image => {
                        // any tap dismisses the image and returns to the listing.
                        current_screen = Screen::Files;
                        needs_redraw = true;
                    }
                    Screen::Notes => {
                        if back_button_hit(sx, sy) {
                            current_screen = Screen::Home;
                            needs_redraw = true;
                        } else if notes::scroll_up_hit(sx, sy) {
                            if notes_scroll > 0 {
                                notes_scroll = notes_scroll.saturating_sub(notes::VISIBLE);
                                notes::draw_note_list(&mut display, &notes_entries, notes_scroll);
                                display.flush_partial_fast(notes::list_native_rect()).ok();
                            }
                        } else if notes::scroll_down_hit(sx, sy) {
                            if notes_scroll + notes::VISIBLE < notes_entries.len() {
                                notes_scroll += notes::VISIBLE;
                                notes::draw_note_list(&mut display, &notes_entries, notes_scroll);
                                display.flush_partial_fast(notes::list_native_rect()).ok();
                            }
                        } else if notes::new_hit(sx, sy) {
                            match notes::next_name(&notes_entries) {
                                Some(name) => {
                                    note_name = name;
                                    note_text.clear();
                                    kb_symbols = false;
                                    kb_shift = false;
                                    note_delete_armed = false;
                                    current_screen = Screen::NoteEdit;
                                    needs_redraw = true;
                                }
                                None => {
                                    notes_status = String::from("notes full");
                                    notes::draw_notes_footer(&mut display, &notes_status);
                                    display.flush_partial_fast(notes::footer_native_rect()).ok();
                                }
                            }
                        } else if let Some(i) =
                            notes::list_hit(sx, sy, notes_entries.len(), notes_scroll)
                        {
                            if let Some(entry) = notes_entries.get(i) {
                                note_name = entry.name.clone();
                                kb_symbols = false;
                                kb_shift = false;
                                note_delete_armed = false;
                                // the editor draws only after the note's text is
                                // read (below), mirroring the reader.
                                note_dirty = true;
                                current_screen = Screen::NoteEdit;
                            }
                        }
                    }
                    Screen::NoteEdit => {
                        if back_button_hit(sx, sy) {
                            notes::save(&bus, &note_name, &note_text);
                            // back to the list, rescanned so the saved note's
                            // preview is current.
                            notes_dirty = true;
                            current_screen = Screen::Notes;
                        } else if notes::delete_hit(sx, sy) {
                            if note_delete_armed {
                                notes::delete(&bus, &note_name);
                                notes_dirty = true;
                                current_screen = Screen::Notes;
                            } else {
                                note_delete_armed = true;
                                notes::draw_delete_button(&mut display, true);
                                display.flush_partial_fast(notes::delete_native_rect()).ok();
                            }
                        } else {
                            // any tap besides the second delete tap disarms it.
                            if note_delete_armed {
                                note_delete_armed = false;
                                notes::draw_delete_button(&mut display, false);
                                display.flush_partial_fast(notes::delete_native_rect()).ok();
                            }
                            if let Some(key) = keyboard::hit(sx, sy, kb_symbols, kb_shift) {
                                match key {
                                    Key::Shift => {
                                        kb_shift = !kb_shift;
                                        keyboard::draw(&mut display, kb_symbols, kb_shift, "RET");
                                        display.flush_partial_fast(keyboard::native_rect()).ok();
                                    }
                                    Key::Symbols => {
                                        kb_symbols = !kb_symbols;
                                        keyboard::draw(&mut display, kb_symbols, kb_shift, "RET");
                                        display.flush_partial_fast(keyboard::native_rect()).ok();
                                    }
                                    other => {
                                        match other {
                                            Key::Char(c) if note_text.len() < notes::NOTE_MAX => {
                                                note_text.push(c)
                                            }
                                            Key::Space if note_text.len() < notes::NOTE_MAX => {
                                                note_text.push(' ')
                                            }
                                            Key::Enter if note_text.len() < notes::NOTE_MAX => {
                                                note_text.push('\n')
                                            }
                                            Key::Backspace => {
                                                note_text.pop();
                                            }
                                            _ => {}
                                        }
                                        notes::draw_note_text(&mut display, &note_text);
                                        display
                                            .flush_partial_fast(notes::text_area_native_rect())
                                            .ok();
                                    }
                                }
                            }
                        }
                    }
                    Screen::Reader => {
                        if back_button_hit(sx, sy) {
                            if let Some(doc) = &reader_doc {
                                doc.save(&bus);
                            }
                            // returning to the shelf rescans so the just-read
                            // book's progress is up to date (cache-fast).
                            if reader_return == Screen::Library {
                                library.invalidate();
                            }
                            current_screen = reader_return;
                            needs_redraw = true;
                        } else if let Some(doc) = &mut reader_doc {
                            let changed = match tap_zone(sx, sy) {
                                Tap::Prev => doc.prev_page(),
                                Tap::Next => doc.next_page(),
                                Tap::None => false,
                            };
                            if changed {
                                needs_redraw = true;
                            }
                        }
                    }
                    Screen::Settings => match settings_page::menu_hit(sx, sy) {
                        Some(MenuHit::Back) => {
                            current_screen = Screen::Home;
                            needs_redraw = true;
                        }
                        Some(MenuHit::System) => {
                            current_screen = Screen::SettingsSystem;
                            needs_redraw = true;
                        }
                        Some(MenuHit::Reader) => {
                            current_screen = Screen::SettingsReader;
                            needs_redraw = true;
                        }
                        Some(MenuHit::Wifi) => {
                            wifi_pw_mode = false;
                            // keep the previous scan list (and its status) cached;
                            // only prompt to scan when there is nothing to show.
                            if wifi_networks.is_empty() {
                                wifi_status = String::from("tap Scan to find networks");
                            }
                            current_screen = Screen::SettingsWifi;
                            needs_redraw = true;
                        }
                        None => {}
                    },
                    Screen::SettingsSystem => match settings_page::system::hit_test(sx, sy) {
                        Some(settings_page::system::Hit::Back) => {
                            current_screen = Screen::Settings;
                            needs_redraw = true;
                        }
                        Some(settings_page::system::Hit::TzMinus) => {
                            settings.tz_offset_hours = (settings.tz_offset_hours - 1).max(-12);
                            settings_dirty = true;
                            settings_page::system::redraw_tz(
                                &mut display,
                                settings.tz_offset_hours,
                            );
                            display
                                .flush_partial_fast(settings_page::system::tz_value_rect())
                                .ok();
                            last_status_minute =
                                refresh_statusbar_clock(&mut display, &mut clock, &settings);
                        }
                        Some(settings_page::system::Hit::TzPlus) => {
                            settings.tz_offset_hours = (settings.tz_offset_hours + 1).min(14);
                            settings_dirty = true;
                            settings_page::system::redraw_tz(
                                &mut display,
                                settings.tz_offset_hours,
                            );
                            display
                                .flush_partial_fast(settings_page::system::tz_value_rect())
                                .ok();
                            last_status_minute =
                                refresh_statusbar_clock(&mut display, &mut clock, &settings);
                        }
                        Some(settings_page::system::Hit::ToggleFormat) => {
                            settings.time_24h = !settings.time_24h;
                            settings_dirty = true;
                            settings_page::system::redraw_format(&mut display, settings.time_24h);
                            display
                                .flush_partial_fast(settings_page::system::format_button_rect())
                                .ok();
                            last_status_minute =
                                refresh_statusbar_clock(&mut display, &mut clock, &settings);
                        }
                        Some(settings_page::system::Hit::CycleIcons) => {
                            settings.icon_style = settings.icon_style.next();
                            settings_dirty = true;
                            settings_page::system::redraw_icons(&mut display, &settings);
                            display
                                .flush_partial_fast(settings_page::system::icons_button_rect())
                                .ok();
                        }
                        Some(settings_page::system::Hit::CycleIconSize) => {
                            settings.icon_size = settings.icon_size.next();
                            settings_dirty = true;
                            settings_page::system::redraw_icon_size(&mut display, &settings);
                            display
                                .flush_partial_fast(settings_page::system::icon_size_button_rect())
                                .ok();
                        }
                        None => {}
                    },
                    Screen::SettingsReader => match settings_page::reader::hit_test(sx, sy) {
                        Some(settings_page::reader::Hit::Back) => {
                            current_screen = Screen::Settings;
                            needs_redraw = true;
                        }
                        Some(settings_page::reader::Hit::CycleFontSize) => {
                            settings.reader_font_size = settings.reader_font_size.next();
                            settings_dirty = true;
                            settings_page::reader::redraw_font_size(&mut display, &settings);
                            display
                                .flush_partial_fast(settings_page::reader::font_size_button_rect())
                                .ok();
                        }
                        Some(settings_page::reader::Hit::CycleFontFamily) => {
                            settings.reader_font_family = settings.reader_font_family.next();
                            settings_dirty = true;
                            settings_page::reader::redraw_family(&mut display, &settings);
                            display
                                .flush_partial_fast(settings_page::reader::family_button_rect())
                                .ok();
                        }
                        Some(settings_page::reader::Hit::CycleSpacing) => {
                            settings.reader_line_spacing = settings.reader_line_spacing.next();
                            settings_dirty = true;
                            settings_page::reader::redraw_spacing(&mut display, &settings);
                            display
                                .flush_partial_fast(settings_page::reader::spacing_button_rect())
                                .ok();
                        }
                        None => {}
                    },
                    Screen::SettingsWifi => {
                        if wifi_pw_mode {
                            if back_button_hit(sx, sy) {
                                // cancel password entry, back to the status view.
                                wifi_pw_mode = false;
                                wifi_status = String::from("join cancelled");
                                needs_redraw = true;
                            } else if let Some(key) = keyboard::hit(sx, sy, kb_symbols, kb_shift) {
                                match key {
                                    Key::Shift => {
                                        kb_shift = !kb_shift;
                                        keyboard::draw(&mut display, kb_symbols, kb_shift, "SAVE");
                                        display.flush_partial_fast(keyboard::native_rect()).ok();
                                    }
                                    Key::Symbols => {
                                        kb_symbols = !kb_symbols;
                                        keyboard::draw(&mut display, kb_symbols, kb_shift, "SAVE");
                                        display.flush_partial_fast(keyboard::native_rect()).ok();
                                    }
                                    Key::Enter => {
                                        // leave the keyboard and attempt the join on
                                        // the next pass (so "connecting..." paints).
                                        wifi_pw_mode = false;
                                        wifi_join_dirty = true;
                                        wifi_status = String::from("connecting...");
                                        needs_redraw = true;
                                    }
                                    other => {
                                        match other {
                                            Key::Char(c) if wifi_pw_buf.len() < 63 => {
                                                wifi_pw_buf.push(c)
                                            }
                                            Key::Space if wifi_pw_buf.len() < 63 => {
                                                wifi_pw_buf.push(' ')
                                            }
                                            Key::Backspace => {
                                                wifi_pw_buf.pop();
                                            }
                                            _ => {}
                                        }
                                        settings_page::wifi::redraw_password(
                                            &mut display,
                                            &wifi_pw_buf,
                                        );
                                        display
                                            .flush_partial_fast(
                                                settings_page::wifi::password_box_rect(),
                                            )
                                            .ok();
                                    }
                                }
                            }
                        } else {
                            match settings_page::wifi::status_hit(sx, sy, &wifi_networks, &settings)
                            {
                                Some(settings_page::wifi::Hit::Back) => {
                                    current_screen = Screen::Settings;
                                    needs_redraw = true;
                                }
                                Some(settings_page::wifi::Hit::Scan) => {
                                    wifi_status = String::from("scanning...");
                                    wifi_scan_dirty = true;
                                    needs_redraw = true;
                                }
                                Some(settings_page::wifi::Hit::Sync) => {
                                    if settings.wifi_ssid().is_empty() {
                                        wifi_status =
                                            String::from("no saved network - join one first");
                                    } else {
                                        wifi_status = String::from("checking internet...");
                                        wifi_sync_dirty = true;
                                    }
                                    needs_redraw = true;
                                }
                                Some(settings_page::wifi::Hit::Network(i)) => {
                                    if let Some(entry) = wifi_networks.get(i) {
                                        if let Some(saved) =
                                            settings.saved_wifi_password(&entry.ssid)
                                        {
                                            // already a saved network: reconnect
                                            // with the stored password instead of
                                            // re-prompting.
                                            wifi_pw_ssid = entry.ssid.clone();
                                            wifi_pw_buf = String::from(saved);
                                            wifi_reconnect = true;
                                            wifi_join_dirty = true;
                                            wifi_status = String::from("reconnecting...");
                                        } else {
                                            wifi_pw_ssid = entry.ssid.clone();
                                            wifi_pw_buf.clear();
                                            wifi_reconnect = false;
                                            if entry.secured {
                                                // secured: collect the passphrase first.
                                                kb_symbols = false;
                                                kb_shift = false;
                                                wifi_pw_mode = true;
                                                wifi_status =
                                                    String::from("enter password, then SAVE");
                                            } else {
                                                // open network: join straight away.
                                                wifi_join_dirty = true;
                                                wifi_status = String::from("connecting...");
                                            }
                                        }
                                        needs_redraw = true;
                                    }
                                }
                                Some(settings_page::wifi::Hit::Forget(i)) => {
                                    if let Some(entry) = wifi_networks.get(i) {
                                        settings.forget_wifi(&entry.ssid);
                                        // persist immediately, like a join: the
                                        // user expects the credentials gone.
                                        settings.save();
                                        settings_dirty = false;
                                        wifi_status = format!("forgot {}", entry.ssid);
                                        needs_redraw = true;
                                    }
                                }
                                None => {}
                            }
                        }
                    }
                    Screen::Music => {
                        if back_button_hit(sx, sy) {
                            current_screen = Screen::Home;
                            needs_redraw = true;
                        } else if let Some(button) = music::hit(sx, sy) {
                            // a control press keeps the page up and reports progress
                            // on the bottom status line, then ok/error when done.
                            music_command = Some(button);
                            music_inline = true;
                            music_anchor = None;
                            music_status = Some("contacting server...");
                            music_status_ticks = 0;
                            music::draw_status(&mut display, "contacting server...");
                            display.flush_partial_fast(music::status_rect()).ok();
                            // no needs_redraw: keep the page, fetch on this pass.
                            music.invalidate();
                        } else {
                            // a tap elsewhere refreshes the whole page.
                            music_command = None;
                            music_inline = false;
                            music_status = None;
                            music.refresh(music::View::Loading);
                            music_anchor = None;
                            needs_redraw = true;
                        }
                    }
                    Screen::Environment => {
                        if back_button_hit(sx, sy) {
                            current_screen = Screen::Home;
                            needs_redraw = true;
                        } else {
                            // a tap anywhere else re-fetches the latest reading.
                            env.refresh(environment::View::Loading);
                            needs_redraw = true;
                        }
                    }
                    Screen::Weather => {
                        if back_button_hit(sx, sy) {
                            current_screen = Screen::Home;
                            needs_redraw = true;
                        } else {
                            // a tap anywhere else re-fetches the latest forecast.
                            weather.refresh(weather::View::Loading);
                            needs_redraw = true;
                        }
                    }
                    Screen::Library => {
                        if back_button_hit(sx, sy) {
                            current_screen = Screen::Home;
                            needs_redraw = true;
                        } else if let library::View::Ready(books) = &library.view {
                            if library::scroll_up_hit(sx, sy) {
                                if library_scroll >= library::VISIBLE {
                                    library_scroll -= library::VISIBLE;
                                    needs_redraw = true;
                                }
                            } else if library::scroll_down_hit(sx, sy) {
                                if library_scroll + library::VISIBLE < books.len() {
                                    library_scroll += library::VISIBLE;
                                    needs_redraw = true;
                                }
                            } else if let Some(idx) =
                                library::card_hit(sx, sy, library_scroll, books.len())
                            {
                                reader_path = String::from(books[idx].path());
                                reader_dirty = true;
                                reader_return = Screen::Library;
                                current_screen = Screen::Reader;
                            }
                        }
                    }
                    #[cfg(feature = "gps")]
                    Screen::Gps => {
                        if back_button_hit(sx, sy) {
                            current_screen = Screen::Home;
                            needs_redraw = true;
                        } else if download_button_hit(sx, sy) {
                            // pre-download the surrounding area on the next pass.
                            gps_download = true;
                        } else if fullscreen_button_hit(sx, sy) {
                            // open the fullscreen map at native zoom.
                            full_zoom = 0;
                            current_screen = Screen::MapFull;
                            needs_redraw = true;
                        } else {
                            // a tap anywhere else forces a map refresh for the
                            // current fix. panel-only (no full-page redraw): the
                            // fetch block below reloads the cell and repaints just
                            // the panel.
                            gps_map.invalidate();
                        }
                    }
                    #[cfg(feature = "gps")]
                    Screen::MapFull => match full_touch(sx, sy) {
                        FullAction::Back => {
                            current_screen = Screen::Gps;
                            needs_redraw = true;
                        }
                        FullAction::ZoomOut => {
                            if full_zoom < FULL_ZOOM_MAX {
                                full_zoom += 1;
                                needs_redraw = true;
                            }
                        }
                        FullAction::ZoomIn => {
                            if full_zoom > FULL_ZOOM_MIN {
                                full_zoom -= 1;
                                needs_redraw = true;
                            }
                        }
                        // re-center on the current fix (re-render at same zoom).
                        FullAction::Recenter => needs_redraw = true,
                    },
                    _ => {
                        if back_button_hit(sx, sy) {
                            current_screen = Screen::Home;
                            needs_redraw = true;
                        }
                    }
                }
            }
            Some(_) => {}
            None => touch_active = false,
        }

        // persist any settings change once, after leaving the settings screens
        // (menu or any sub-page), instead of writing flash on every tap or on
        // navigation between the sub-pages (flash wear).
        if settings_dirty
            && !matches!(
                current_screen,
                Screen::Settings
                    | Screen::SettingsSystem
                    | Screen::SettingsReader
                    | Screen::SettingsWifi
            )
        {
            settings.save();
            settings_dirty = false;
        }

        // (re)load the directory listing when the browser is opened or navigates
        // into another folder. mounting the card is self-contained (it steals and
        // releases SPI2), so this can run any time we're on the Files screen
        // without conflicting with the radio, which is dropped off-screen.
        if current_screen == Screen::Files && files_dirty {
            files_dirty = false;
            files_scroll = 0;
            match load_dir(&bus, &files_path) {
                Ok(entries) => {
                    files_status = format!("{} items", entries.len());
                    files_entries = entries;
                }
                Err(e) => {
                    esp_println::println!("files: load {files_path} failed: {e:?}");
                    files_entries = Vec::new();
                    files_status = String::from("SD read failed");
                }
            }
            needs_redraw = true;
        }

        // (re)load the notes listing when the notes page is opened or a note
        // was saved. self-contained mount, mirrors the file browser.
        if current_screen == Screen::Notes && notes_dirty {
            notes_dirty = false;
            notes_scroll = 0;
            match notes::load_list(&bus) {
                Ok(entries) => {
                    notes_status = format!("{} notes", entries.len());
                    notes_entries = entries;
                }
                Err(e) => {
                    esp_println::println!("notes: load listing failed: {e:?}");
                    notes_entries = Vec::new();
                    notes_status = String::from("SD read failed");
                }
            }
            needs_redraw = true;
        }

        // read the tapped note's text when the editor is opened (mirrors the
        // reader). on failure fall back to the list with the error on its
        // footer.
        if current_screen == Screen::NoteEdit && note_dirty {
            note_dirty = false;
            match notes::load_note(&bus, &note_name) {
                Ok(text) => note_text = text,
                Err(e) => {
                    esp_println::println!("notes: read {note_name} failed: {e:?}");
                    notes_status = String::from("note read failed");
                    current_screen = Screen::Notes;
                }
            }
            needs_redraw = true;
        }

        // (re)load and paginate the open text file when the reader is entered.
        // mounting the card is self-contained, so this is safe any time we're on
        // the Reader screen. progress is restored to the saved page, clamped to
        // the document's length.
        if current_screen == Screen::Reader && reader_dirty {
            reader_dirty = false;
            match load_document(&bus, &reader_path, settings.reader_style()) {
                Ok(doc) => {
                    reader_doc = Some(doc);
                    reader_status.clear();
                }
                Err(msg) => {
                    reader_doc = None;
                    reader_status = msg;
                }
            }
            needs_redraw = true;
        }

        // queue the now-playing fetch to the wifi task when the music page is
        // opened or refreshed; the result comes back through the event router
        // above while the ui keeps running. the `!needs_redraw` guard lets the
        // "loading" view paint first.
        if current_screen == Screen::Music
            && music.is_dirty()
            && !needs_redraw
            && wifi_pending.is_none()
        {
            let command = music_command.take();
            let inline = music_inline;
            music_inline = false;
            if wifi::send(saved_request(
                &settings,
                wifi::Op::Music {
                    command: command.map(music::Button::command),
                },
            )) {
                music.clear();
                wifi_pending = Some(Pending::Music { inline, command });
            }
        }

        if current_screen == Screen::Environment
            && env.is_dirty()
            && !needs_redraw
            && wifi_pending.is_none()
        {
            let path = environment::path();
            if wifi::send(saved_request(
                &settings,
                wifi::Op::Get {
                    host: wifi::Host::Server,
                    path: String::from(path.as_str()),
                    max_body: 8192,
                },
            )) {
                env.clear();
                wifi_pending = Some(Pending::Environment);
            }
        }

        // load the map for the gps page's current fix. cache-first: the cell the
        // fix falls in is read straight off the sd card (no wifi) when present,
        // and only fetched (via the wifi task) + cached on a miss. without a fix
        // there is no center to request a map for.
        #[cfg(feature = "gps")]
        if current_screen == Screen::Gps && gps_map.is_dirty() && !needs_redraw {
            match last_fix {
                Some(fix) => {
                    let cell = map_cell(fix.lat(), fix.lon());
                    let cache_path = map_cache_path(cell.key());
                    let cached = {
                        let card = SdCard::new(&bus).ok();
                        card.as_ref()
                            .and_then(|c| c.read_file(cache_path.as_str()).ok())
                            .map(|body| parse_map(&body, cell.dx(), cell.dy()))
                            .and_then(|view| match view {
                                // parse_map only returns Ready or Error. a cached
                                // tile that no longer decodes is corrupt (e.g. a
                                // truncated download from an older build): delete
                                // it so the fetch below can replace it.
                                MapView::Error => {
                                    if let Some(c) = &card {
                                        c.delete_file(cache_path.as_str()).ok();
                                    }
                                    None
                                }
                                view => Some(view),
                            })
                    };
                    match cached {
                        Some(view) => {
                            gps_map.clear();
                            gps_map.view = view;
                            // refresh just the map panel (grayscale, panel-only)
                            // rather than a full page redraw, so following the
                            // fix flashes only the panel.
                            refresh_map_panel(&mut display, &gps_map.view);
                            gps_marker_ticks = 0;
                            gps_shown_cell = Some(cell.key());
                            gps_shown_offset = (cell.dx(), cell.dy());
                        }
                        None if wifi_pending.is_none() => {
                            let path = map_request_path(cell.key());
                            if wifi::send(saved_request(
                                &settings,
                                wifi::Op::Get {
                                    host: wifi::Host::Map,
                                    path: String::from(path.as_str()),
                                    max_body: MAP_MAX_BYTES,
                                },
                            )) {
                                gps_map.clear();
                                wifi_pending = Some(Pending::MapTile {
                                    key: cell.key(),
                                    dx: cell.dx(),
                                    dy: cell.dy(),
                                });
                                gps_map.view = MapView::Loading;
                                refresh_map_panel(&mut display, &gps_map.view);
                            }
                        }
                        // wifi task busy: keep the dirty flag and retry on a
                        // later pass.
                        None => {}
                    }
                }
                None => {
                    gps_map.clear();
                    gps_map.view = MapView::NoFix;
                    refresh_map_panel(&mut display, &gps_map.view);
                    gps_shown_cell = None;
                }
            }
        }

        // pre-download the cells around the current fix into the sd cache so the
        // map keeps working offline while moving. the whole area is fetched by
        // the wifi task in a single session (one radio bring-up), streaming each
        // tile back to be cached as it arrives, so the ui stays responsive.
        #[cfg(feature = "gps")]
        if current_screen == Screen::Gps && gps_download && wifi_pending.is_none() {
            gps_download = false;
            if let Some(fix) = last_fix {
                let card = match SdCard::new(&bus) {
                    Ok(c) => Some(c),
                    Err(e) => {
                        esp_println::println!("gps: save-area sd init failed: {e:?}");
                        None
                    }
                };
                // keep only the cells not already cached. list the cache dir
                // once (uppercased names) and membership-test each tile, rather
                // than a per-tile lookup — far cheaper for a few hundred tiles.
                let missing: Vec<(u32, String)> = match &card {
                    Some(c) => {
                        c.create_dir_all(MAP_CACHE_DIR).ok();
                        let existing: BTreeSet<String> = c
                            .list_dir(MAP_CACHE_DIR)
                            .map(|entries| {
                                entries
                                    .into_iter()
                                    .map(|e| e.name.to_ascii_uppercase())
                                    .collect()
                            })
                            .unwrap_or_default();
                        map_area_tiles(fix.lat(), fix.lon(), DOWNLOAD_RADIUS)
                            .into_iter()
                            .filter(|(key, _)| {
                                !existing.contains(map_cache_filename(*key).as_str())
                            })
                            .collect()
                    }
                    None => Vec::new(),
                };

                if card.is_some() {
                    // download only the not-yet-cached cells; skip the wifi
                    // session entirely when the whole area is already saved.
                    if missing.is_empty() {
                        esp_println::println!("gps: area already cached, nothing to download");
                        let km2 = area_km2(fix.lat(), DOWNLOAD_RADIUS);
                        gps_map.view = MapView::Saved { km2, new_tiles: 0 };
                        refresh_map_panel(&mut display, &gps_map.view);
                        Timer::after_millis(1500).await;
                        // reload the current cell on the next pass.
                        gps_map.invalidate();
                    } else {
                        let total = missing.len();
                        gps_map.view = MapView::Downloading(total);
                        refresh_map_panel(&mut display, &gps_map.view);
                        if wifi::send(saved_request(
                            &settings,
                            wifi::Op::DownloadMaps {
                                tiles: missing,
                                max_body: MAP_MAX_BYTES,
                            },
                        )) {
                            wifi_pending = Some(Pending::MapArea {
                                total,
                                done: 0,
                                km2: area_km2(fix.lat(), DOWNLOAD_RADIUS),
                            });
                        }
                    }
                } else {
                    // no card to cache onto; just reload the panel.
                    gps_map.invalidate();
                }
            }
        }

        // the weather page fetches a public forecast for the device's current
        // GPS position; without a fix there are no coordinates to query for.
        if current_screen == Screen::Weather && weather.is_dirty() && !needs_redraw {
            #[cfg(feature = "gps")]
            match last_fix {
                Some(fix) => {
                    if wifi_pending.is_none() {
                        let path = weather::path(fix.lat(), fix.lon());
                        if wifi::send(saved_request(
                            &settings,
                            wifi::Op::Get {
                                host: wifi::Host::External(weather::HOST),
                                path: String::from(path.as_str()),
                                // a forecast response is a few kB; cap well
                                // before the heap is at risk.
                                max_body: 16 * 1024,
                            },
                        )) {
                            weather.clear();
                            wifi_pending = Some(Pending::Weather);
                        }
                    }
                }
                None => {
                    weather.clear();
                    weather.view = weather::View::NoFix;
                    needs_redraw = true;
                }
            }
            #[cfg(not(feature = "gps"))]
            {
                weather.clear();
                weather.view = weather::View::NoFix;
                needs_redraw = true;
            }
        }

        // queue a scan for nearby access points when Scan is tapped on the wifi
        // settings page; the "scanning..." status stays up until the ScanDone
        // event lands.
        if current_screen == Screen::SettingsWifi
            && wifi_scan_dirty
            && !needs_redraw
            && wifi_pending.is_none()
            && wifi::send(wifi::Request {
                ssid: String::new(),
                password: String::new(),
                op: wifi::Op::Scan,
            })
        {
            wifi_scan_dirty = false;
            wifi_pending = Some(Pending::Scan);
        }

        // queue a join of the selected network with the entered credentials.
        // the credentials are only persisted on a successful connect (see the
        // JoinDone event) so a wrong passphrase is never saved.
        if current_screen == Screen::SettingsWifi
            && wifi_join_dirty
            && !needs_redraw
            && wifi_pending.is_none()
        {
            let reconnect = wifi_reconnect;
            wifi_reconnect = false;
            if wifi::send(wifi::Request {
                ssid: wifi_pw_ssid.clone(),
                password: wifi_pw_buf.clone(),
                op: wifi::Op::Join,
            }) {
                wifi_join_dirty = false;
                wifi_pending = Some(Pending::Join { reconnect });
            }
        }

        // queue a clock re-sync over the saved network when Sync clock is
        // tapped, doubling as an internet-access check. the `!needs_redraw`
        // guard lets the "checking internet..." status paint first; the
        // outcome is reported on the wifi page when the TimeSynced event lands
        // (see the SyncCheck arm above).
        if current_screen == Screen::SettingsWifi
            && wifi_sync_dirty
            && !needs_redraw
            && wifi_pending.is_none()
            && wifi::send(saved_request(&settings, wifi::Op::SyncTime))
        {
            wifi_sync_dirty = false;
            wifi_pending = Some(Pending::SyncCheck);
        }

        // scan the SD card for the book shelf when the library is opened (or
        // re-entered from the reader). the `!needs_redraw` guard lets the
        // "scanning" view paint first, since the scan can be slow on first run
        // (it parses each new epub and decodes its cover). self-contained mount,
        // so it never conflicts with the radio, which is dropped off-screen.
        if current_screen == Screen::Library && library.is_dirty() && !needs_redraw {
            library.clear();
            library.view = library::load_library(&bus);
            needs_redraw = true;
        }

        // the radio listens only while the lora screen is open: bring it up in
        // receive mode on entry, set it to standby and drop it on leave (frees
        // SPI2 for the SD wallpaper at sleep and avoids drawing rx current).
        // `radio_tried` keeps a failed init from re-resetting the chip every
        // pass; it re-arms when the screen is left.
        if current_screen == Screen::Lora {
            if radio.is_none() && !radio_tried {
                radio_tried = true;
                match make_radio(&bus) {
                    Ok(mut r) => {
                        if let Err(e) = r.start_receive() {
                            esp_println::println!("lora: start rx failed: {e}");
                        }
                        radio = Some(r);
                    }
                    Err(e) => {
                        esp_println::println!("lora: init failed: {e}");
                        lora_status = String::from("radio init failed");
                    }
                }
            }
        } else {
            radio_tried = false;
            if let Some(mut r) = radio.take() {
                r.standby().ok();
            }
        }

        // poll for an incoming packet (cheap: just a dio1 read until one lands)
        // and append it to the received log.
        if current_screen == Screen::Lora {
            if let Some(r) = &mut radio {
                let mut rx = [0u8; 255];
                if let Ok(Some(n)) = r.poll_receive(&mut rx) {
                    let rssi = r.rssi();
                    let text = core::str::from_utf8(&rx[..n]).unwrap_or("<binary>");
                    esp_println::println!("lora rx: {text} ({rssi} dBm)");
                    // stamp each entry with its local receive time.
                    lora_recv.push(format!("{}{text}", lora_stamp(&mut clock, &settings)));
                    if lora_recv.len() > LIST_MAX {
                        lora_recv.remove(0);
                    }
                    lora_status = format!("received {n} bytes ({rssi} dBm)");
                    draw_list(&mut display, RECV_Y, "received", &lora_recv);
                    display.flush_partial_fast(received_native_rect()).ok();
                    draw_lora_status(&mut display, &lora_status);
                    display.flush_partial_fast(lora_status_native_rect()).ok();
                }
            }
        }

        // keep the GPS UART drained with a single non-blocking read per pass
        // and refresh the readout periodically. one read every ~50ms keeps
        // the 128-byte FIFO (~133ms at 9600 baud) from overflowing without
        // blocking the touch poll above.
        #[cfg(feature = "gps")]
        if let Some(ref mut g) = gps {
            g.update().ok();
            if let Some(f) = current_fix(g) {
                last_fix = Some(f);
            }

            if current_screen == Screen::Gps && !needs_redraw {
                gps_refresh += 1;
                if gps_refresh >= GPS_REFRESH_TICKS {
                    gps_refresh = 0;
                    draw_gps_data(&mut display, g, last_fix);
                    display.flush_partial_fast(gps_data_native_rect()).ok();
                }

                // follow the fix on the map: reload when it crosses into a new
                // tile, otherwise nudge the marker within the current tile on a
                // slow cadence so the grayscale panel isn't constantly flashing.
                if let Some(fix) = last_fix {
                    let cell = map_cell(fix.lat(), fix.lon());
                    match gps_shown_cell {
                        Some(shown) if shown != cell.key() => {
                            // crossed a tile boundary: reload (cache-first) on the
                            // next pass via the map fetch block above.
                            gps_map.invalidate();
                        }
                        Some(_) => {
                            gps_marker_ticks = gps_marker_ticks.saturating_add(1);
                            let moved = (cell.dx() - gps_shown_offset.0).abs()
                                + (cell.dy() - gps_shown_offset.1).abs();
                            if gps_marker_ticks >= MARKER_REFRESH_TICKS && moved >= MARKER_MOVE_PX {
                                gps_marker_ticks = 0;
                                if let MapView::Ready { dx, dy, .. } = &mut gps_map.view {
                                    *dx = cell.dx();
                                    *dy = cell.dy();
                                }
                                refresh_map_panel(&mut display, &gps_map.view);
                                gps_shown_offset = (cell.dx(), cell.dy());
                            }
                        }
                        None => {}
                    }
                }
            }
        }

        // yield to the executor (instead of a busy-wait) so the wifi task can
        // run while the ui idles; also lets the cpu rest between passes.
        Timer::after_millis(50).await;
    }

    // let an in-flight wifi session finish before tearing down, so deep sleep
    // never races an active radio. a bulk download is cancelled (it stops after
    // the current tile); remaining events are drained and dropped.
    if wifi_pending.is_some() {
        wifi::cancel();
        while let Ok(WifiEvent::Tile { .. }) =
            with_timeout(Duration::from_secs(30), wifi::next_event()).await
        {}
    }

    // sleep was requested from the Sleep screen. turn the front light off,
    // paint the screensaver (e-ink retains it with the panel unpowered), then
    // enter deep sleep. the boot button wakes the chip, which resets and
    // re-runs main() from the top.
    light.set_brightness(0);
    // if we slept straight from the lora screen the radio still holds SPI2 and
    // the lora CS; standby and release them so the SD wallpaper can use the bus.
    if let Some(mut r) = radio.take() {
        r.standby().ok();
    }
    // persist reading progress if we slept straight from the reader.
    if current_screen == Screen::Reader {
        if let Some(doc) = &reader_doc {
            doc.save(&bus);
        }
    }
    // persist the open note if we slept straight from the editor.
    if current_screen == Screen::NoteEdit {
        notes::save(&bus, &note_name, &note_text);
    }
    // persist the user's brightness so it is restored on the next boot.
    settings.brightness = brightness;
    settings.save();

    // full power-off requested from the sleep screen: paint a notice, then cut
    // the battery FET via the PMIC. with USB connected the board may stay
    // powered, so halt afterwards rather than falling into the deep-sleep path.
    if power_off {
        let pct = display.battery_percentage().unwrap_or(0);
        display.clear().ok();
        draw_power_off_screen(&mut display, pct);
        display.flush(DrawMode::BlackOnWhite).expect("to flush");
        if let Err(e) = t5s3_epaper_core::power::shutdown(display) {
            esp_println::println!("power: shutdown failed: {e:?}");
        }
        loop {
            core::hint::spin_loop();
        }
    }

    // remember where we were so wake lands on the same screen. single-threaded,
    // so writing the RTC-backed static is sound.
    unsafe {
        LAST_SCREEN = current_screen.to_index();
    }
    display.clear().ok();
    // pick a random wallpaper from the SD card; fall back to the drawn
    // screensaver if the folder is missing or has no usable .bmp files.
    if !show_wallpaper(&mut display, &bus) {
        let pct = display.battery_percentage().unwrap_or(0);
        draw_screensaver(&mut display, pct);
    }
    display.flush(DrawMode::BlackOnWhite).expect("to flush");
    // hand LPWR back from the clock for the deep-sleep path.
    display.deep_sleep(clock.into_inner(), input_ctl, None)
}
