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
mod widgets;
mod wifi;

use alloc::{format, string::String, vec::Vec};

use embassy_executor::Spawner;
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
    lora::Lora,
    pin_config,
    sdcard::DirectoryEntry,
    Clock,
    Display,
    DrawMode,
    FrontLight,
};
#[cfg(feature = "gps")]
use t5s3_epaper_core::{gps::Gps, gps_pin_config};

#[cfg(feature = "gps")]
use crate::pages::gps::{
    current_fix,
    draw_gps_data,
    draw_map,
    gps_data_native_rect,
    map_path,
    parse_map,
    GpsFix,
    MapView,
    GPS_REFRESH_TICKS,
    MAP_MAX_BYTES,
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
        http_get,
        http_get_from,
        music_session,
        scan as wifi_scan,
        set_utc_time,
        sync_time,
        try_connect,
        ScanEntry,
        RESYNC_INTERVAL_SECS,
    },
};

esp_bootloader_esp_idf::esp_app_desc!();

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

#[esp_rtos::main]
async fn main(_spawner: Spawner) -> ! {
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

    // a cold boot needs a fresh time sync; a wake from deep sleep keeps the RTC.
    let woke = t5s3_epaper_core::power::wake_status().woke_from_deep_sleep();
    let mut clock = Clock::new(peripherals.LPWR);

    // user settings (timezone, time format, brightness, reader font size) read
    // from the NVS flash partition; falls back to build-time defaults on first
    // boot or if the stored blob is missing/invalid.
    let mut settings = Settings::load();

    let mut display = Display::new(
        pin_config!(peripherals),
        peripherals.I2C0,
        peripherals.DMA_CH0,
        peripherals.LCD_CAM,
        peripherals.RMT,
    )
    .expect("to initialize display");

    display.set_rotation(DisplayRotation::Rotate270);

    let mut light =
        FrontLight::new(peripherals.LEDC, peripherals.GPIO11).expect("to initialize front light");
    light.set_brightness(settings.brightness);

    let delay = Delay::new();
    display.power_on().expect("to power on");
    delay.delay_millis(10);
    display.clear().expect("to clear");

    // one SPI2 bus shared by the SD card and the LoRa radio, owned here and lent
    // out by reference so only a single owner ever exists — no more per-access
    // steal()s of SPI2 and the sclk/mosi/miso lines.
    let bus = t5s3_epaper_core::sdcard::shared_bus(
        peripherals.SPI2,
        peripherals.GPIO14,
        peripherals.GPIO13,
        peripherals.GPIO21,
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

    // on a cold boot, sync the clock over wifi (best effort, with a timeout so
    // it still boots when offline), then the radio powers down. on wake the RTC
    // already holds the time, so we skip wifi for a fast resume.
    if !woke {
        Text::with_alignment(
            "syncing clock over wifi...",
            Point::new(SCREEN_W / 2, 400),
            MonoTextStyle::new(&FONT_9X15, Gray4::BLACK),
            Alignment::Center,
        )
        .draw(&mut display)
        .ok();
        display.flush(DrawMode::BlackOnWhite).expect("to flush");

        match sync_time(
            peripherals.WIFI,
            settings.wifi_ssid(),
            settings.wifi_password(),
        )
        .await
        {
            Some(unix) => set_utc_time(&mut clock, unix),
            None => esp_println::println!("clock: wifi/ntp sync failed, time unavailable"),
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
    let mut files_dirty = false;
    // path of the .bmp currently shown full-screen by the image viewer.
    let mut image_path = String::new();

    // reader state: the open text file, its paginated document (None if the
    // load failed), the current page, and a flag that the document needs
    // (re)loading from the card on the next pass (mirrors `files_dirty`).
    let mut reader_path = String::new();
    let mut reader_doc: Option<ReaderDoc> = None;
    let mut reader_dirty = false;
    // why the open failed, shown on the reader screen when `reader_doc` is None.
    let mut reader_status = String::new();

    // music / environment page state: the last fetched view and a flag that a
    // fresh fetch from noot-server is needed on the next pass (set on entry and
    // on tap-to-refresh, mirrors `files_dirty`).
    let mut music_view = music::View::Loading;
    let mut music_dirty = false;
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
    let mut env_view = environment::View::Loading;
    let mut env_dirty = false;

    // weather page state: mirrors the environment page (last fetched view plus a
    // flag that a fresh fetch from noot-server is needed on the next pass, set on
    // entry and on tap-to-refresh).
    let mut weather_view = weather::View::Loading;
    let mut weather_dirty = false;

    // library (reader shelf) state: the scanned books, the scroll offset into
    // them, and a flag that a fresh scan is needed on the next pass (set on entry
    // and on returning from the reader, mirrors `files_dirty`).
    let mut library_view = library::View::Loading;
    let mut library_scroll: usize = 0;
    let mut library_dirty = false;

    // wifi settings page state: the last scan results, a status line, and flags
    // that a scan or a join should run on the next pass (mirrors env/weather).
    // `pw_mode` swaps the page to the password keyboard for the network being
    // joined (`pw_ssid`), accumulating the typed passphrase in `pw_buf`.
    let mut wifi_networks: Vec<ScanEntry> = Vec::new();
    let mut wifi_status = String::from("tap Scan to find networks");
    let mut wifi_scan_dirty = false;
    let mut wifi_join_dirty = false;
    let mut wifi_pw_mode = false;
    let mut wifi_pw_ssid = String::new();
    let mut wifi_pw_buf = String::new();
    // set when the pending join is a reconnect to the already-saved network using
    // its stored password: on failure we fall back to the password keyboard so
    // stale credentials can be re-entered, rather than reporting a plain error.
    let mut wifi_reconnect = false;

    // the screen the reader returns to on Back: the file browser or the library,
    // depending on where the book was opened from.
    let mut reader_return = Screen::Files;

    #[cfg(feature = "gps")]
    let mut gps_refresh: u16 = 0;
    // last good position, kept so a dropped fix shows the previous coordinates
    // (marked stale) instead of blanking out.
    #[cfg(feature = "gps")]
    let mut last_fix: Option<GpsFix> = None;

    // the gps page's map panel: the last fetched view plus a flag that a fresh
    // fetch is needed on the next pass (set on entry and on tap-to-refresh).
    #[cfg(feature = "gps")]
    let mut gps_map = MapView::Loading;
    #[cfg(feature = "gps")]
    let mut gps_map_dirty = false;

    loop {
        if needs_redraw {
            let pct = display.battery_percentage().unwrap_or(0);
            let now = status_time(&mut clock, settings.tz_offset_hours);

            display.clear().ok();
            // the image viewer paints full-screen, so it skips the status bar.
            if current_screen != Screen::Image {
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
                            draw_map(&mut display, &gps_map);
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
                    let (voltage, temp, uptime, since_sync) = read_info(&mut display, &mut clock);
                    draw_info_screen(&mut display, voltage, temp, uptime, since_sync);
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
                            kb_symbols,
                            kb_shift,
                        );
                    } else {
                        settings_page::wifi::draw_status(
                            &mut display,
                            settings.wifi_ssid(),
                            &wifi_status,
                            &wifi_networks,
                        );
                    }
                }
                Screen::Music => music::draw_screen(&mut display, &music_view, music_status),
                Screen::Environment => environment::draw_screen(&mut display, &env_view),
                Screen::Weather => weather::draw_screen(&mut display, &weather_view),
                Screen::Library => {
                    library::draw_screen(&mut display, &library_view, library_scroll)
                }
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
        // stays current without switching pages. the image viewer hides the
        // status bar, so skip it there.
        if !needs_redraw && current_screen != Screen::Image {
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
                let (voltage, temp, uptime, since_sync) = read_info(&mut display, &mut clock);
                draw_info_values(&mut display, voltage, temp, uptime, since_sync);
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
                        music_dirty = true;
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
        if clock.now_us() / 1_000_000 >= last_resync_secs + RESYNC_INTERVAL_SECS {
            esp_println::println!("clock: periodic re-sync");
            let wifi = unsafe { esp_hal::peripherals::WIFI::steal() };
            if let Some(unix) =
                sync_time(wifi, settings.wifi_ssid(), settings.wifi_password()).await
            {
                set_utc_time(&mut clock, unix);
            }
            last_resync_secs = clock.now_us() / 1_000_000;
            needs_redraw = true;
        }

        // poll touch/buttons every pass so input stays responsive. the GPS
        // work below is non-blocking, so it never stalls this poll. a transient
        // read error shouldn't reboot the ui, so log it and retry next pass.
        let input = match display.input() {
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
                                // the server pages paint a "loading" view now,
                                // then fetch over wifi on the next pass.
                                Screen::Music => {
                                    music_view = music::View::Loading;
                                    music_command = None;
                                    music_inline = false;
                                    music_status = None;
                                    music_anchor = None;
                                    music_dirty = true;
                                    needs_redraw = true;
                                }
                                Screen::Environment => {
                                    env_view = environment::View::Loading;
                                    env_dirty = true;
                                    needs_redraw = true;
                                }
                                Screen::Weather => {
                                    weather_view = weather::View::Loading;
                                    weather_dirty = true;
                                    needs_redraw = true;
                                }
                                // the shelf paints a "scanning" view now, then
                                // scans the card on the next pass.
                                Screen::Library => {
                                    library_view = library::View::Loading;
                                    library_scroll = 0;
                                    library_dirty = true;
                                    needs_redraw = true;
                                }
                                // the gps page paints its map panel as "loading"
                                // now, then fetches over wifi on the next pass.
                                #[cfg(feature = "gps")]
                                Screen::Gps => {
                                    gps_map = MapView::Loading;
                                    gps_map_dirty = true;
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
                    Screen::Reader => {
                        if back_button_hit(sx, sy) {
                            if let Some(doc) = &reader_doc {
                                doc.save(&bus);
                            }
                            // returning to the shelf rescans so the just-read
                            // book's progress is up to date (cache-fast).
                            library_dirty = reader_return == Screen::Library;
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
                            match settings_page::wifi::status_hit(sx, sy, wifi_networks.len()) {
                                Some(settings_page::wifi::Hit::Back) => {
                                    current_screen = Screen::Settings;
                                    needs_redraw = true;
                                }
                                Some(settings_page::wifi::Hit::Scan) => {
                                    wifi_status = String::from("scanning...");
                                    wifi_scan_dirty = true;
                                    needs_redraw = true;
                                }
                                Some(settings_page::wifi::Hit::Network(i)) => {
                                    if let Some(entry) = wifi_networks.get(i) {
                                        if entry.ssid == settings.wifi_ssid() {
                                            // already the saved network: reconnect
                                            // with the stored password instead of
                                            // re-prompting.
                                            wifi_pw_ssid = entry.ssid.clone();
                                            wifi_pw_buf = String::from(settings.wifi_password());
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
                                            } else {
                                                // open network: join straight away.
                                                wifi_join_dirty = true;
                                                wifi_status = String::from("connecting...");
                                            }
                                        }
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
                            music_dirty = true;
                        } else {
                            // a tap elsewhere refreshes the whole page.
                            music_command = None;
                            music_inline = false;
                            music_status = None;
                            music_view = music::View::Loading;
                            music_anchor = None;
                            music_dirty = true;
                            needs_redraw = true;
                        }
                    }
                    Screen::Environment => {
                        if back_button_hit(sx, sy) {
                            current_screen = Screen::Home;
                            needs_redraw = true;
                        } else {
                            // a tap anywhere else re-fetches the latest reading.
                            env_view = environment::View::Loading;
                            env_dirty = true;
                            needs_redraw = true;
                        }
                    }
                    Screen::Weather => {
                        if back_button_hit(sx, sy) {
                            current_screen = Screen::Home;
                            needs_redraw = true;
                        } else {
                            // a tap anywhere else re-fetches the latest forecast.
                            weather_view = weather::View::Loading;
                            weather_dirty = true;
                            needs_redraw = true;
                        }
                    }
                    Screen::Library => {
                        if back_button_hit(sx, sy) {
                            current_screen = Screen::Home;
                            needs_redraw = true;
                        } else if let library::View::Ready(books) = &library_view {
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
                        } else {
                            // a tap anywhere else re-fetches the map.
                            gps_map = MapView::Loading;
                            gps_map_dirty = true;
                            needs_redraw = true;
                        }
                    }
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

        // fetch the now-playing data from noot-server when the music page is
        // opened or refreshed. the `!needs_redraw` guard lets the "loading" view
        // paint first, since the wifi bring-up below blocks the loop for several
        // seconds. (steal WIFI: the previous controller was dropped, so the
        // peripheral is free to re-init; the radio powers down when http_get
        // returns.)
        if current_screen == Screen::Music && music_dirty && !needs_redraw {
            music_dirty = false;
            let command = music_command.take();
            let inline = music_inline;
            music_inline = false;
            let wifi = unsafe { esp_hal::peripherals::WIFI::steal() };
            let snapshot = music_session(
                wifi,
                settings.wifi_ssid(),
                settings.wifi_password(),
                command.map(music::Button::command),
            )
            .await;
            match snapshot {
                Some(snap) => {
                    music_view = music::build_view(&snap.json, snap.cover.as_deref());
                    // anchor the local position tick to this fetch.
                    music_anchor = music::playback(&music_view)
                        .map(|p| (p.base_secs, p.duration_secs, clock.now_us()));
                    music_refresh = 0;
                    music_status_ticks = 0;
                    if !inline || command.is_some_and(music::Button::changes_art) {
                        // a (re)load, or a track change (next/previous), redraws
                        // fully so the album art re-renders. draw_screen shows the
                        // "ok" status on the repainted page when inline.
                        music_status = if inline { Some("ok") } else { None };
                        needs_redraw = true;
                    } else if command.is_some_and(music::Button::changes_display) {
                        // play/pause: repaint just the band below the art.
                        music_status = Some("ok");
                        music::redraw_body(&mut display, &music_view, Some("ok"));
                        display.flush_partial_fast(music::below_art_rect()).ok();
                    } else {
                        // volume: only the status line needs updating.
                        music_status = Some("ok");
                        music::draw_status(&mut display, "ok");
                        display.flush_partial_fast(music::status_rect()).ok();
                    }
                }
                None if inline => {
                    // leave the page as-is; just report the failure.
                    music_status = Some("error");
                    music_status_ticks = 0;
                    music::draw_status(&mut display, "error");
                    display.flush_partial_fast(music::status_rect()).ok();
                }
                None => {
                    music_status = None;
                    music_view = music::View::Error;
                    needs_redraw = true;
                }
            }
        }

        if current_screen == Screen::Environment && env_dirty && !needs_redraw {
            env_dirty = false;
            let path = environment::path();
            let wifi = unsafe { esp_hal::peripherals::WIFI::steal() };
            env_view = match http_get(
                wifi,
                settings.wifi_ssid(),
                settings.wifi_password(),
                path.as_str(),
                8192,
            )
            .await
            {
                Some(body) => environment::parse(&body),
                None => environment::View::Error,
            };
            needs_redraw = true;
        }

        // fetch the map for the gps page's current fix. mirrors the weather
        // fetch: without a fix there is no center to request a map for.
        #[cfg(feature = "gps")]
        if current_screen == Screen::Gps && gps_map_dirty && !needs_redraw {
            gps_map_dirty = false;
            gps_map = match last_fix {
                Some(fix) => {
                    let path = map_path(fix.lat(), fix.lon());
                    let wifi = unsafe { esp_hal::peripherals::WIFI::steal() };
                    match http_get(
                        wifi,
                        settings.wifi_ssid(),
                        settings.wifi_password(),
                        path.as_str(),
                        MAP_MAX_BYTES,
                    )
                    .await
                    {
                        Some(body) => parse_map(&body),
                        None => MapView::Error,
                    }
                }
                None => MapView::NoFix,
            };
            needs_redraw = true;
        }

        if current_screen == Screen::Weather && weather_dirty && !needs_redraw {
            weather_dirty = false;
            // the weather page fetches a public forecast for the device's current
            // GPS position; without a fix there are no coordinates to query for.
            #[cfg(feature = "gps")]
            {
                weather_view = match last_fix {
                    Some(fix) => {
                        let path = weather::path(fix.lat(), fix.lon());
                        let wifi = unsafe { esp_hal::peripherals::WIFI::steal() };
                        match http_get_from(
                            wifi,
                            settings.wifi_ssid(),
                            settings.wifi_password(),
                            weather::HOST,
                            path.as_str(),
                        )
                        .await
                        {
                            Some(body) => weather::parse(&body),
                            None => weather::View::Error,
                        }
                    }
                    None => weather::View::NoFix,
                };
            }
            #[cfg(not(feature = "gps"))]
            {
                weather_view = weather::View::NoFix;
            }
            needs_redraw = true;
        }

        // scan for nearby access points when Scan is tapped on the wifi settings
        // page. the `!needs_redraw` guard lets the "scanning..." view paint first,
        // since the scan brings wifi up and blocks the loop for a few seconds.
        // (steal WIFI: any previous controller was dropped, so the peripheral is
        // free; the radio powers down when `wifi_scan` returns.)
        if current_screen == Screen::SettingsWifi && wifi_scan_dirty && !needs_redraw {
            wifi_scan_dirty = false;
            let wifi = unsafe { esp_hal::peripherals::WIFI::steal() };
            match wifi_scan(wifi).await {
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
            needs_redraw = true;
        }

        // attempt to join the selected network with the entered credentials, then
        // power the radio down. only persists the credentials on a successful
        // connect so a wrong passphrase is never saved. same `!needs_redraw`
        // guard so the "connecting..." view paints before the blocking join.
        if current_screen == Screen::SettingsWifi && wifi_join_dirty && !needs_redraw {
            wifi_join_dirty = false;
            let reconnect = wifi_reconnect;
            wifi_reconnect = false;
            let wifi = unsafe { esp_hal::peripherals::WIFI::steal() };
            if try_connect(wifi, &wifi_pw_ssid, &wifi_pw_buf).await {
                settings.set_wifi(&wifi_pw_ssid, &wifi_pw_buf);
                settings_dirty = true;
                wifi_status = format!("connected: {wifi_pw_ssid}");
            } else if reconnect {
                // the saved password no longer works: drop into the keyboard
                // (pre-filled with it) so the user can correct it.
                kb_symbols = false;
                kb_shift = false;
                wifi_pw_mode = true;
                wifi_status = String::from("reconnect failed - re-enter password");
            } else {
                wifi_status = String::from("join failed - check password");
            }
            needs_redraw = true;
        }

        // scan the SD card for the book shelf when the library is opened (or
        // re-entered from the reader). the `!needs_redraw` guard lets the
        // "scanning" view paint first, since the scan can be slow on first run
        // (it parses each new epub and decodes its cover). self-contained mount,
        // so it never conflicts with the radio, which is dropped off-screen.
        if current_screen == Screen::Library && library_dirty && !needs_redraw {
            library_dirty = false;
            library_view = library::load_library(&bus);
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
            }
        }

        delay.delay_millis(50);
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
    display.deep_sleep(clock.into_inner(), None)
}
