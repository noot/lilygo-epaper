use core::fmt::Write as _;

use embedded_graphics::{
    mono_font::{ascii::FONT_6X10, MonoTextStyle},
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::Text,
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use t5s3_epaper_core::{gps::Gps, Display};

use crate::{fmt::FmtBuf, layout::screen_to_native_rect};

// loop ticks between GPS readout refreshes (~50ms per tick)
pub(crate) const GPS_REFRESH_TICKS: u16 = 30;

// a position fix worth keeping on screen after the live fix drops, so a brief
// signal loss shows the last known position instead of blanking to "--".
#[derive(Clone, Copy)]
pub(crate) struct GpsFix {
    lat: f64,
    lng: f64,
    alt: f32,
    speed: f32,
    hdop: f32,
    vdop: f32,
    sats: u32,
}

impl GpsFix {
    pub(crate) fn lat(&self) -> f64 {
        self.lat
    }

    pub(crate) fn lon(&self) -> f64 {
        self.lng
    }
}

// snapshot the current fix, or None if the receiver has no position right now.
pub(crate) fn current_fix(gps: &Gps<'_>) -> Option<GpsFix> {
    let (lat, lng) = gps.location()?;
    Some(GpsFix {
        lat,
        lng,
        alt: gps.altitude().unwrap_or(0.0),
        speed: gps.speed_over_ground().unwrap_or(0.0),
        hdop: gps.hdop().unwrap_or(0.0),
        vdop: gps.vdop().unwrap_or(0.0),
        sats: gps.fix_satellites().unwrap_or(0),
    })
}

pub(crate) fn draw_gps_data(display: &mut Display, gps: &Gps<'_>, last_fix: Option<GpsFix>) {
    let small = MonoTextStyle::new(&FONT_6X10, Gray4::BLACK);
    let x = 60;
    let line_h = 28;
    let mut y = 200;

    // fill the data area with white before drawing text, matching the
    // u8g2 WithBackground pattern used in the working gps example.
    // this ensures previous text is explicitly cleared in the framebuffer
    // so the DU waveform sees clean transitions.
    Rectangle::new(Point::new(30, 170), Size::new(480, 300))
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(display)
        .ok();

    let module_name = match gps.module() {
        t5s3_epaper_core::gps::Module::L76K => "L76K",
        t5s3_epaper_core::gps::Module::MiaM10Q => "MIA-M10Q",
    };

    let in_view = gps.satellites_in_view();
    let current = current_fix(gps);
    // when the live fix drops, keep showing the last known position rather than
    // blanking it. the DU partial-refresh waveform is 1-bit, so we flag the
    // stale data with text ("re-acquiring" / "(last)") rather than a grey tone
    // it can't render.
    let stale = current.is_none() && last_fix.is_some();
    let shown = current.or(last_fix);
    let mark = if stale { "  (last)" } else { "" };

    let mut buf = FmtBuf::<48>::new();

    write!(buf, "Module: {}", module_name).ok();
    Text::new(buf.as_str(), Point::new(x, y), small)
        .draw(display)
        .ok();
    y += line_h;

    buf.reset();
    match current {
        Some(f) => {
            let fix_str = match gps.fix_type() {
                Some(nmea::sentences::FixType::Gps) => "GPS",
                Some(nmea::sentences::FixType::DGps) => "DGPS",
                Some(nmea::sentences::FixType::Rtk) => "RTK",
                Some(nmea::sentences::FixType::FloatRtk) => "float RTK",
                Some(_) => "other",
                None => "fix",
            };
            write!(
                buf,
                "Fix:    {} ({} used, {} in view)",
                fix_str, f.sats, in_view
            )
            .ok();
        }
        None if stale => {
            write!(buf, "Fix:    re-acquiring ({in_view} in view)").ok();
        }
        None => {
            write!(buf, "Fix:    no fix ({in_view} in view)").ok();
        }
    }
    Text::new(buf.as_str(), Point::new(x, y), small)
        .draw(display)
        .ok();
    y += line_h + 10;

    match shown {
        Some(f) => {
            buf.reset();
            write!(buf, "Lat:    {:.6}{}", f.lat, mark).ok();
            Text::new(buf.as_str(), Point::new(x, y), small)
                .draw(display)
                .ok();
            y += line_h;

            buf.reset();
            write!(buf, "Lon:    {:.6}{}", f.lng, mark).ok();
            Text::new(buf.as_str(), Point::new(x, y), small)
                .draw(display)
                .ok();
            y += line_h;

            buf.reset();
            write!(buf, "Alt:    {:.1} m", f.alt).ok();
            Text::new(buf.as_str(), Point::new(x, y), small)
                .draw(display)
                .ok();
            y += line_h;

            buf.reset();
            write!(buf, "Speed:  {:.1} kn", f.speed).ok();
            Text::new(buf.as_str(), Point::new(x, y), small)
                .draw(display)
                .ok();
            y += line_h;

            buf.reset();
            write!(buf, "HDOP:   {:.1}   VDOP: {:.1}", f.hdop, f.vdop).ok();
            Text::new(buf.as_str(), Point::new(x, y), small)
                .draw(display)
                .ok();
        }
        None => {
            Text::new("Lat:    --", Point::new(x, y), small)
                .draw(display)
                .ok();
            y += line_h;
            Text::new("Lon:    --", Point::new(x, y), small)
                .draw(display)
                .ok();
            y += line_h;
            Text::new("Alt:    --", Point::new(x, y), small)
                .draw(display)
                .ok();
            y += line_h;
            Text::new("Speed:  --", Point::new(x, y), small)
                .draw(display)
                .ok();
            y += line_h;
            Text::new("HDOP:   --   VDOP: --", Point::new(x, y), small)
                .draw(display)
                .ok();
        }
    }
}

pub(crate) fn gps_data_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(30, 170, 480, 300)
}
