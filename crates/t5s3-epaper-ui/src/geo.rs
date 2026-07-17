//! Great-circle distance and bearing between GPS positions, for the mesh
//! peer table ("1.2km NE") and map markers. Coordinates arrive as degrees
//! scaled by 10^7 (the mesh wire format).

use libm::{atan2, cos, sin, sqrt};

const EARTH_RADIUS_M: f64 = 6_371_000.0;

fn radians(deg_e7: i32) -> f64 {
    f64::from(deg_e7) / 1e7 * core::f64::consts::PI / 180.0
}

/// Haversine distance in meters.
pub(crate) fn distance_m(from_e7: (i32, i32), to_e7: (i32, i32)) -> f64 {
    let (lat1, lon1) = (radians(from_e7.0), radians(from_e7.1));
    let (lat2, lon2) = (radians(to_e7.0), radians(to_e7.1));
    let dlat = lat2 - lat1;
    let dlon = lon2 - lon1;
    let a = sin(dlat / 2.0) * sin(dlat / 2.0)
        + cos(lat1) * cos(lat2) * sin(dlon / 2.0) * sin(dlon / 2.0);
    2.0 * EARTH_RADIUS_M * atan2(sqrt(a), sqrt(1.0 - a))
}

/// Initial great-circle bearing from `from` to `to`, as an 8-point compass
/// label.
pub(crate) fn compass8(from_e7: (i32, i32), to_e7: (i32, i32)) -> &'static str {
    let (lat1, lon1) = (radians(from_e7.0), radians(from_e7.1));
    let (lat2, lon2) = (radians(to_e7.0), radians(to_e7.1));
    let dlon = lon2 - lon1;
    let y = sin(dlon) * cos(lat2);
    let x = cos(lat1) * sin(lat2) - sin(lat1) * cos(lat2) * cos(dlon);
    let bearing_deg = atan2(y, x) * 180.0 / core::f64::consts::PI;
    // bucket into 45-degree sectors centered on the compass points
    let sector = ((bearing_deg + 360.0 + 22.5) / 45.0) as usize % 8;
    ["N", "NE", "E", "SE", "S", "SW", "W", "NW"][sector]
}

/// "870m" below a kilometer, "1.2km" above, "87km" past ten.
pub(crate) fn distance_label(meters: f64) -> alloc::string::String {
    use alloc::format;
    if meters < 1000.0 {
        format!("{}m", meters as u32)
    } else if meters < 10_000.0 {
        format!(
            "{}.{}km",
            (meters / 1000.0) as u32,
            ((meters / 100.0) as u32) % 10
        )
    } else {
        format!("{}km", (meters / 1000.0) as u32)
    }
}
