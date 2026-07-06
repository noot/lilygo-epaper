use alloc::boxed::Box;
use core::fmt::Write as _;

use embedded_graphics::{
    image::Image,
    mono_font::{
        ascii::{FONT_9X15, FONT_9X18_BOLD},
        MonoTextStyle,
    },
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use heapless::{String as HString, Vec as HVec};
use serde::Deserialize;
use t5s3_epaper_core::Display;
use tinybmp::Bmp;
use u8g2_fonts::{
    fonts,
    types::{FontColor, HorizontalAlignment, VerticalPosition},
    FontRenderer,
};

use crate::{
    datetime::{weekday, DAY_NAMES},
    fmt::FmtBuf,
    layout::SCREEN_W,
    widgets::{centered, draw_back_button},
};

// the public forecast api the page fetches from. keyless and reachable over
// plain http (no TLS), which is all this device's http client speaks.
pub(crate) const HOST: &str = "api.open-meteo.com";

// number of forecast days requested and drawn.
const FORECAST_DAYS: usize = 3;

// large font for the current temperature. fub42's full latin-1 set includes the
// degree sign, so the hero reading can show "18°C".
static TEMP_FONT: FontRenderer = FontRenderer::new::<fonts::u8g2_font_fub42_tf>();

// the `current` block of an open-meteo forecast response.
#[derive(Deserialize)]
pub(crate) struct Current {
    temperature_2m: f32,
    relative_humidity_2m: f32,
    apparent_temperature: f32,
    weather_code: u16,
    wind_speed_10m: f32,
}

// the `daily` block: parallel arrays, one entry per forecast day.
#[derive(Deserialize)]
pub(crate) struct Daily {
    time: HVec<HString<12>, FORECAST_DAYS>,
    weather_code: HVec<u16, FORECAST_DAYS>,
    temperature_2m_max: HVec<f32, FORECAST_DAYS>,
    temperature_2m_min: HVec<f32, FORECAST_DAYS>,
}

// the parts of an open-meteo forecast response the page uses. latitude and
// longitude are echoed back (snapped to the model grid) and shown as the place
// label, since the api returns no place name.
#[derive(Deserialize)]
pub(crate) struct Weather {
    latitude: f32,
    longitude: f32,
    current: Current,
    daily: Daily,
}

// what the weather page is currently showing. Ready is boxed: it is much larger
// than the other variants (the forecast arrays plus their date strings).
pub(crate) enum View {
    Loading,
    // no GPS position yet, so there are no coordinates to query for.
    NoFix,
    Ready(Box<Weather>),
    Error,
}

// the open-meteo request path for a position, asking for the current conditions
// and a short daily forecast in the location's local timezone.
pub(crate) fn path(lat: f64, lon: f64) -> FmtBuf<288> {
    let mut buf = FmtBuf::<288>::new();
    write!(
        buf,
        "/v1/forecast?latitude={lat:.4}&longitude={lon:.4}\
         &current=temperature_2m,relative_humidity_2m,apparent_temperature,weather_code,wind_speed_10m\
         &daily=weather_code,temperature_2m_max,temperature_2m_min\
         &forecast_days=3&timezone=auto"
    )
    .ok();
    buf
}

// parse an open-meteo forecast response body into a view.
pub(crate) fn parse(body: &[u8]) -> View {
    match serde_json_core::from_slice::<Weather>(body) {
        Ok((weather, _)) => View::Ready(Box::new(weather)),
        Err(_) => View::Error,
    }
}

// a short description of a WMO weather interpretation code (as returned in
// `weather_code`), matching open-meteo's documented code table.
fn condition(code: u16) -> &'static str {
    match code {
        0 => "Clear",
        1 => "Mainly clear",
        2 => "Partly cloudy",
        3 => "Overcast",
        45 | 48 => "Fog",
        51 | 53 | 55 => "Drizzle",
        56 | 57 => "Freezing drizzle",
        61 | 63 | 65 => "Rain",
        66 | 67 => "Freezing rain",
        71 | 73 | 75 => "Snow",
        77 => "Snow grains",
        80..=82 => "Rain showers",
        85 | 86 => "Snow showers",
        95 => "Thunderstorm",
        96 | 99 => "Thunderstorm, hail",
        _ => "Unknown",
    }
}

// the abbreviated weekday name for an ISO "YYYY-MM-DD" date, or "" if it does
// not parse. used to label forecast columns.
fn day_label(iso: &str) -> &'static str {
    let parsed = (|| {
        let year = iso.get(0..4)?.parse::<i64>().ok()?;
        let month = iso.get(5..7)?.parse::<u32>().ok()?;
        let day = iso.get(8..10)?.parse::<u32>().ok()?;
        Some(weekday(year, month, day))
    })();
    match parsed {
        Some(dow) => &DAY_NAMES[dow][..3],
        None => "",
    }
}

pub(crate) fn draw_screen(display: &mut Display, view: &View) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    draw_back_button(display);
    Text::with_alignment(
        "Weather",
        Point::new(SCREEN_W / 2, 120),
        bold,
        Alignment::Center,
    )
    .draw(display)
    .ok();

    let label = MonoTextStyle::new(&FONT_9X15, Gray4::new(4));
    match view {
        View::Loading => centered(display, "loading...", 360, label),
        View::NoFix => centered(display, "waiting for GPS fix...", 360, label),
        View::Error => centered(display, "could not reach weather service", 360, label),
        View::Ready(w) => {
            // place label: the coordinates the forecast is for.
            let mut loc = FmtBuf::<32>::new();
            let (la, ns) = if w.latitude >= 0.0 {
                (w.latitude, 'N')
            } else {
                (-w.latitude, 'S')
            };
            let (lo, ew) = if w.longitude >= 0.0 {
                (w.longitude, 'E')
            } else {
                (-w.longitude, 'W')
            };
            write!(loc, "{la:.3} {ns}, {lo:.3} {ew}").ok();
            centered(display, loc.as_str(), 160, label);

            draw_current(display, w);
            draw_forecast(display, &w.daily);
        }
    }
}

// the current conditions: a large temperature, the condition text, then a row
// of feels-like / humidity / wind.
fn draw_current(display: &mut Display, w: &Weather) {
    draw_weather_icon(display, w.current.weather_code, SCREEN_W / 2, 232, false);

    let mut temp = FmtBuf::<12>::new();
    write!(temp, "{:.0}°C", w.current.temperature_2m).ok();
    TEMP_FONT
        .render_aligned(
            temp.as_str(),
            Point::new(SCREEN_W / 2, 346),
            VerticalPosition::Baseline,
            HorizontalAlignment::Center,
            FontColor::Transparent(Gray4::BLACK),
            display,
        )
        .ok();

    centered(
        display,
        condition(w.current.weather_code),
        390,
        MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK),
    );

    let mut feels = FmtBuf::<12>::new();
    write!(feels, "{:.0} C", w.current.apparent_temperature).ok();
    let mut hum = FmtBuf::<12>::new();
    write!(hum, "{:.0} %", w.current.relative_humidity_2m).ok();
    let mut wind = FmtBuf::<12>::new();
    write!(wind, "{:.0} km/h", w.current.wind_speed_10m).ok();

    let cols = [
        (90, "feels like", feels.as_str()),
        (270, "humidity", hum.as_str()),
        (450, "wind", wind.as_str()),
    ];
    let label = MonoTextStyle::new(&FONT_9X15, Gray4::new(4));
    let value = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    for (cx, name, val) in cols {
        Text::with_alignment(name, Point::new(cx, 470), label, Alignment::Center)
            .draw(display)
            .ok();
        Text::with_alignment(val, Point::new(cx, 502), value, Alignment::Center)
            .draw(display)
            .ok();
    }
}

// a divider and the daily forecast, each column day / condition / hi / lo.
fn draw_forecast(display: &mut Display, daily: &Daily) {
    Rectangle::new(Point::new(30, 560), Size::new((SCREEN_W - 60) as u32, 2))
        .into_styled(PrimitiveStyle::with_fill(Gray4::new(8)))
        .draw(display)
        .ok();

    let label = MonoTextStyle::new(&FONT_9X15, Gray4::new(4));
    let value = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    centered(display, "forecast", 600, label);

    let days = daily.time.len().min(FORECAST_DAYS);
    let step = SCREEN_W / (days.max(1) as i32 + 1);
    for i in 0..days {
        let cx = step * (i as i32 + 1);
        Text::with_alignment(
            day_label(daily.time[i].as_str()),
            Point::new(cx, 636),
            value,
            Alignment::Center,
        )
        .draw(display)
        .ok();
        draw_weather_icon(display, daily.weather_code[i], cx, 680, true);
        let mut hilo = FmtBuf::<16>::new();
        write!(
            hilo,
            "{:.0} / {:.0}",
            daily.temperature_2m_max[i], daily.temperature_2m_min[i]
        )
        .ok();
        Text::with_alignment(hilo.as_str(), Point::new(cx, 734), value, Alignment::Center)
            .draw(display)
            .ok();
    }
}

// the small set of lucide pictograms a WMO code maps to.
#[derive(Clone, Copy)]
enum Sky {
    Clear,
    PartlyCloudy,
    Cloudy,
    Fog,
    Rain,
    Snow,
    Storm,
}

fn sky(code: u16) -> Sky {
    match code {
        0 | 1 => Sky::Clear,
        2 => Sky::PartlyCloudy,
        3 => Sky::Cloudy,
        45 | 48 => Sky::Fog,
        51..=57 | 61..=67 | 80..=82 => Sky::Rain,
        71..=77 | 85 | 86 => Sky::Snow,
        95..=99 => Sky::Storm,
        _ => Sky::Cloudy,
    }
}

// blit the lucide weather pictogram for `code` centered at (cx, cy). `small`
// picks the forecast-column size over the current-conditions size.
fn draw_weather_icon(display: &mut Display, code: u16, cx: i32, cy: i32, small: bool) {
    let Ok(bmp) = Bmp::<Gray4>::from_slice(icon_bytes(sky(code), small)) else {
        return;
    };
    let dim = bmp.size();
    let top_left = Point::new(cx - dim.width as i32 / 2, cy - dim.height as i32 / 2);
    Image::new(&bmp, top_left).draw(display).ok();
}

fn icon_bytes(sky: Sky, small: bool) -> &'static [u8] {
    macro_rules! pick {
        ($cond:literal) => {{
            let bytes: &'static [u8] = if small {
                include_bytes!(concat!("../../assets/icons/weather/small/", $cond, ".bmp"))
            } else {
                include_bytes!(concat!("../../assets/icons/weather/large/", $cond, ".bmp"))
            };
            bytes
        }};
    }
    match sky {
        Sky::Clear => pick!("clear"),
        Sky::PartlyCloudy => pick!("partly"),
        Sky::Cloudy => pick!("cloudy"),
        Sky::Fog => pick!("fog"),
        Sky::Rain => pick!("rain"),
        Sky::Snow => pick!("snow"),
        Sky::Storm => pick!("storm"),
    }
}
