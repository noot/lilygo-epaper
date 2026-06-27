#![no_std]
#![no_main]

extern crate alloc;
extern crate t5s3_epaper_core;

mod datetime;
mod fmt;
mod layout;
mod pages;
mod screen;
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
    sdcard_pin_config,
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
    gps_data_native_rect,
    GpsFix,
    GPS_REFRESH_TICKS,
};
use crate::{
    datetime::{status_date, status_time},
    layout::{touch_to_screen, SCREEN_W},
    pages::{
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
        lora::{
            draw_keyboard,
            draw_list,
            draw_lora_screen,
            draw_lora_status,
            draw_message,
            kb_hit,
            keyboard_native_rect,
            lora_status_native_rect,
            make_radio,
            message_box_native_rect,
            received_native_rect,
            sent_native_rect,
            Key,
            LIST_MAX,
            MSG_MAX,
            RECV_Y,
            SENT_Y,
        },
        sleep::{draw_screensaver, draw_sleep_screen, show_wallpaper, sleep_now_hit},
    },
    screen::Screen,
    widgets::{
        back_button_hit,
        draw_back_button,
        draw_status_bar,
        draw_statusbar_time,
        statusbar_time_rect,
    },
    wifi::{set_local_time, sync_time, RESYNC_INTERVAL_SECS},
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

    let delay = Delay::new();
    display.power_on().expect("to power on");
    delay.delay_millis(10);
    display.clear().expect("to clear");

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

    // timezone offset (hours from UTC) from the TZ_OFFSET_HOURS build env (see
    // .env); defaults to pacific daylight time. used for the initial sync and
    // each periodic re-sync.
    let offset_hours: i64 = option_env!("TZ_OFFSET_HOURS")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(-7);

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

        match sync_time(peripherals.WIFI).await {
            Some(unix) => set_local_time(&mut clock, unix, offset_hours),
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
    let mut brightness: u8 = 0;
    // whether a finger is currently down, so each tap is handled once on press.
    let mut touch_active = false;
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
    let mut radio: Option<Lora<'static>> = None;
    let mut radio_tried = false;
    let mut kb_symbols = false;
    let mut kb_shift = false;
    // ticks since the info page was last refreshed (uptime/temp/voltage).
    let mut info_refresh: u16 = 0;

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

    #[cfg(feature = "gps")]
    let mut gps_refresh: u16 = 0;
    // last good position, kept so a dropped fix shows the previous coordinates
    // (marked stale) instead of blanking out.
    #[cfg(feature = "gps")]
    let mut last_fix: Option<GpsFix> = None;

    loop {
        if needs_redraw {
            let pct = display.battery_percentage().unwrap_or(0);
            let now = status_time(&mut clock);

            display.clear().ok();
            // the image viewer paints full-screen, so it skips the status bar.
            if current_screen != Screen::Image {
                draw_status_bar(&mut display, pct, now);
            }
            match current_screen {
                Screen::Home => draw_home(&mut display, status_date(&mut clock)),
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
                        Some(g) => draw_gps_data(&mut display, g, last_fix),
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
                    if !view_image(&mut display, &image_path) {
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
            }
            display.flush(DrawMode::BlackOnWhite).expect("to flush");
            needs_redraw = false;
            last_status_minute = now.map_or(60, |(_, m)| m);
            info_refresh = 0;

            #[cfg(feature = "gps")]
            {
                gps_refresh = 0;
            }
        }

        // tick the status-bar clock once a minute via a fast partial refresh.
        if !needs_redraw {
            if let Some((h, m)) = status_time(&mut clock) {
                if m != last_status_minute {
                    last_status_minute = m;
                    draw_statusbar_time(&mut display, Some((h, m)));
                    display.flush_partial_fast(statusbar_time_rect()).ok();
                }
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

        // periodically bring wifi back up briefly to re-sync the clock, then it
        // powers down again. correct against RTC drift without leaving the radio
        // on to interfere with gps. (steal WIFI: the previous controller was
        // dropped, so the peripheral is free to re-init.)
        if clock.now_us() / 1_000_000 >= last_resync_secs + RESYNC_INTERVAL_SECS {
            esp_println::println!("clock: periodic re-sync");
            let wifi = unsafe { esp_hal::peripherals::WIFI::steal() };
            if let Some(unix) = sync_time(wifi).await {
                set_local_time(&mut clock, unix, offset_hours);
            }
            last_resync_secs = clock.now_us() / 1_000_000;
            needs_redraw = true;
        }

        // poll touch/buttons every pass so input stays responsive. the GPS
        // work below is non-blocking, so it never stalls this poll.
        let input = display.input().expect("to read input");

        if input.buttons.home && current_screen != Screen::Home {
            current_screen = Screen::Home;
            needs_redraw = true;
        }

        // the auxiliary button sleeps from any screen; the current screen is
        // restored on wake.
        if input.buttons.auxiliary {
            break;
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
                            if current_screen == Screen::Files {
                                files_path = String::from("/");
                                files_dirty = true;
                            } else {
                                needs_redraw = true;
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
                        }
                    }
                    Screen::Lora => {
                        if back_button_hit(sx, sy) {
                            current_screen = Screen::Home;
                            needs_redraw = true;
                        } else if let Some(key) = kb_hit(sx, sy, kb_symbols, kb_shift) {
                            match key {
                                Key::Shift => {
                                    kb_shift = !kb_shift;
                                    draw_keyboard(&mut display, kb_symbols, kb_shift);
                                    display.flush_partial_fast(keyboard_native_rect()).ok();
                                }
                                Key::Symbols => {
                                    kb_symbols = !kb_symbols;
                                    draw_keyboard(&mut display, kb_symbols, kb_shift);
                                    display.flush_partial_fast(keyboard_native_rect()).ok();
                                }
                                Key::Send => {
                                    if lora_message.is_empty() {
                                        lora_status = String::from("nothing to send");
                                    } else if let Some(r) = &mut radio {
                                        match r.transmit(lora_message.as_bytes()) {
                                            Ok(()) => {
                                                esp_println::println!("lora tx: {lora_message}");
                                                lora_status =
                                                    format!("sent {} bytes", lora_message.len());
                                                lora_sent.push(lora_message.clone());
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

        // (re)load the directory listing when the browser is opened or navigates
        // into another folder. mounting the card is self-contained (it steals and
        // releases SPI2), so this can run any time we're on the Files screen
        // without conflicting with the radio, which is dropped off-screen.
        if current_screen == Screen::Files && files_dirty {
            files_dirty = false;
            files_scroll = 0;
            match load_dir(&files_path) {
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

        // the radio listens only while the lora screen is open: bring it up in
        // receive mode on entry, set it to standby and drop it on leave (frees
        // SPI2 for the SD wallpaper at sleep and avoids drawing rx current).
        // `radio_tried` keeps a failed init from re-resetting the chip every
        // pass; it re-arms when the screen is left.
        if current_screen == Screen::Lora {
            if radio.is_none() && !radio_tried {
                radio_tried = true;
                match make_radio() {
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
                    lora_recv.push(String::from(text));
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
    // remember where we were so wake lands on the same screen. single-threaded,
    // so writing the RTC-backed static is sound.
    unsafe {
        LAST_SCREEN = current_screen.to_index();
    }
    display.clear().ok();
    // pick a random wallpaper from the SD card; fall back to the drawn
    // screensaver if the folder is missing or has no usable .bmp files.
    if !show_wallpaper(
        &mut display,
        peripherals.SPI2,
        sdcard_pin_config!(peripherals),
        peripherals.GPIO46,
    ) {
        let pct = display.battery_percentage().unwrap_or(0);
        draw_screensaver(&mut display, pct);
    }
    display.flush(DrawMode::BlackOnWhite).expect("to flush");
    // hand LPWR back from the clock for the deep-sleep path.
    display.deep_sleep(clock.into_inner(), None)
}
