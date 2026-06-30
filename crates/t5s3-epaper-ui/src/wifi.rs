use alloc::{string::String, vec::Vec};
use core::{fmt::Write as _, net::Ipv4Addr};

use embassy_futures::select::{select, Either};
use embassy_net::{
    dns::DnsQueryType,
    tcp::TcpSocket,
    udp::{PacketMetadata, UdpSocket},
    IpAddress,
    IpEndpoint,
    Stack,
    StackResources,
};
use embassy_time::{with_timeout, Duration, Timer};
use embedded_io_async::Write as _;
use esp_hal::rng::Rng;
use esp_radio::wifi::{sta::StationConfig, Config, ControllerConfig, Interface, WifiController};
use t5s3_epaper_core::Clock;

// ── wifi / ntp config (from .env at build time; see the justfile) ────
const SSID: &str = match option_env!("SSID") {
    Some(s) => s,
    None => "changeme",
};
const PASSWORD: &str = match option_env!("PASSWORD") {
    Some(s) => s,
    None => "changeme",
};
// noot-server address (from .env at build time; see the justfile). the music
// and environment pages fetch JSON from it over http.
const SERVER_HOST: &str = match option_env!("SERVER_HOST") {
    Some(s) => s,
    None => "192.168.1.100",
};
const SERVER_PORT: u16 = match option_env!("SERVER_PORT") {
    Some(s) => parse_port(s),
    None => 3009,
};

// parse a decimal port at build time; falls back to 3009 on anything
// unexpected. hand-rolled because u16::from_str_radix is not const-stable.
const fn parse_port(s: &str) -> u16 {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut n: u32 = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b < b'0' || b > b'9' {
            return 3009;
        }
        n = n * 10 + (b - b'0') as u32;
        i += 1;
    }
    if n == 0 || n > u16::MAX as u32 {
        3009
    } else {
        n as u16
    }
}

const NTP_SERVER: &str = "pool.ntp.org";
// seconds between the NTP epoch (1900-01-01) and the unix epoch (1970-01-01).
const NTP_UNIX_DELTA: u64 = 2_208_988_800;
// re-sync the clock over wifi this often. wifi is powered down between syncs so
// it doesn't interfere with gps reception; the RTC only drifts seconds per day.
pub(crate) const RESYNC_INTERVAL_SECS: u64 = 4 * 3600;

// connect to wifi, fetch the current unix time via SNTP, then power the radio
// back down. self-contained: it drives the network stack (`runner`) alongside
// the connect+query work via `select`, so when the query finishes everything
// here drops — and WifiController's Drop deinitialises wifi (radio off), which
// frees the 2.4 GHz band for gps and saves power. returns UTC unix seconds, or
// None on timeout. re-callable for periodic re-sync (steal WIFI again).
pub(crate) async fn sync_time(wifi: esp_hal::peripherals::WIFI<'static>) -> Option<u64> {
    let station_config = Config::Station(
        StationConfig::default()
            .with_ssid(SSID)
            .with_password(PASSWORD.into()),
    );
    let mut controller = WifiController::new(
        wifi,
        ControllerConfig::default().with_initial_config(station_config),
    )
    .ok()?;

    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;
    let mut resources = StackResources::<3>::new();
    let (stack, mut runner) = embassy_net::new(
        Interface::station(),
        embassy_net::Config::dhcpv4(Default::default()),
        &mut resources,
        seed,
    );

    let outcome = select(runner.run(), async {
        controller.connect_async().await.ok()?;
        with_timeout(Duration::from_secs(15), stack.wait_config_up())
            .await
            .ok()?;
        esp_println::println!("wifi: connected");
        for _ in 0..3 {
            if let Some(unix) = sntp_unix_time(stack).await {
                return Some(unix);
            }
            Timer::after(Duration::from_secs(2)).await;
        }
        None
    })
    .await;

    match outcome {
        Either::First(_) => None,
        Either::Second(unix) => unix,
    }
}

// bring wifi up, GET `path` from noot-server, copy the response body into
// `body`, then power the radio back down. mirrors `sync_time`: it drives the
// network stack alongside the connect+request work via `select`, so everything
// drops when the request finishes and WifiController's Drop deinitialises the
// radio. returns the number of body bytes written to `body`, or None on any
// failure.
pub(crate) async fn http_get(
    wifi: esp_hal::peripherals::WIFI<'static>,
    path: &str,
    body: &mut [u8],
) -> Option<usize> {
    let station_config = Config::Station(
        StationConfig::default()
            .with_ssid(SSID)
            .with_password(PASSWORD.into()),
    );
    let mut controller = WifiController::new(
        wifi,
        ControllerConfig::default().with_initial_config(station_config),
    )
    .ok()?;

    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;
    let mut resources = StackResources::<3>::new();
    let (stack, mut runner) = embassy_net::new(
        Interface::station(),
        embassy_net::Config::dhcpv4(Default::default()),
        &mut resources,
        seed,
    );

    let outcome = select(runner.run(), async {
        controller.connect_async().await.ok()?;
        with_timeout(Duration::from_secs(15), stack.wait_config_up())
            .await
            .ok()?;
        with_timeout(Duration::from_secs(10), fetch_body(stack, path, body))
            .await
            .ok()?
    })
    .await;

    match outcome {
        Either::First(_) => None,
        Either::Second(n) => n,
    }
}

// resolve the server address (an IP literal, else a DNS lookup), open a TCP
// connection, send a minimal HTTP/1.1 request, and return the response body.
async fn fetch_body(stack: Stack<'_>, path: &str, body: &mut [u8]) -> Option<usize> {
    let addr = match SERVER_HOST.parse::<Ipv4Addr>() {
        Ok(ip) => IpAddress::Ipv4(ip),
        Err(_) => *stack
            .dns_query(SERVER_HOST, DnsQueryType::A)
            .await
            .ok()?
            .first()?,
    };

    let mut rx = [0u8; 1536];
    let mut tx = [0u8; 1536];
    let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
    socket.set_timeout(Some(Duration::from_secs(8)));
    socket
        .connect(IpEndpoint::new(addr, SERVER_PORT))
        .await
        .ok()?;

    // a minimal HTTP/1.1 GET. Connection: close lets us read until EOF rather
    // than parse the content length.
    let mut req = String::new();
    write!(
        req,
        "GET {path} HTTP/1.1\r\nHost: {SERVER_HOST}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    socket.write_all(req.as_bytes()).await.ok()?;

    // read the whole response into a scratch buffer (headers + body).
    let mut resp = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        match socket.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                resp.extend_from_slice(&chunk[..n]);
                // guard against an unexpectedly large response.
                if resp.len() > 8192 {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    // split headers from body at the blank line, then copy the body out.
    let split = resp.windows(4).position(|w| w == b"\r\n\r\n")?;
    let payload = &resp[split + 4..];
    let len = payload.len().min(body.len());
    body[..len].copy_from_slice(&payload[..len]);
    Some(len)
}

// set the RTC to UTC from an NTP unix timestamp. the timezone offset is applied
// at display time (see `datetime`), so changing the offset takes effect without
// a re-sync and the RTC always holds UTC.
pub(crate) fn set_utc_time(clock: &mut Clock, utc_unix: u64) {
    clock.set_now_us(utc_unix * 1_000_000);
    // record the sync time for the info page's "time since sync".
    unsafe {
        crate::LAST_SYNC_UNIX = utc_unix;
    }
    esp_println::println!("clock: set utc unix={utc_unix}");
}

// query an NTP server over UDP and return the current unix time in seconds.
async fn sntp_unix_time(stack: embassy_net::Stack<'_>) -> Option<u64> {
    let addrs = stack.dns_query(NTP_SERVER, DnsQueryType::A).await.ok()?;
    let server = IpEndpoint::new(*addrs.first()?, 123);

    let mut rx_meta = [PacketMetadata::EMPTY; 4];
    let mut rx_buf = [0u8; 128];
    let mut tx_meta = [PacketMetadata::EMPTY; 4];
    let mut tx_buf = [0u8; 128];
    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    socket.bind(50123).ok()?;

    // minimal SNTP client request: LI=0, VN=3, Mode=3 (client).
    let mut request = [0u8; 48];
    request[0] = 0x1B;
    socket.send_to(&request, server).await.ok()?;

    let mut response = [0u8; 48];
    let (n, _) = socket.recv_from(&mut response).await.ok()?;
    if n < 44 {
        return None;
    }
    // transmit timestamp (seconds since 1900) is at bytes 40..44, big-endian.
    let ntp_secs = u32::from_be_bytes([response[40], response[41], response[42], response[43]]);
    Some((ntp_secs as u64).saturating_sub(NTP_UNIX_DELTA))
}
