use alloc::{string::String, vec::Vec};
use core::{
    fmt::Write as _,
    net::Ipv4Addr,
    sync::atomic::{AtomicBool, Ordering},
};

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
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::{with_timeout, Duration, Timer};
use embedded_io_async::Write as _;
use esp_hal::rng::Rng;
use esp_radio::wifi::{
    scan::ScanConfig,
    sta::StationConfig,
    AuthenticationMethod,
    Config,
    ControllerConfig,
    Interface,
    WifiController,
};
use t5s3_epaper_core::Clock;

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
// how often to retry while the clock has never synced (e.g. wifi was down at
// boot), so it recovers within minutes rather than waiting a full re-sync
// interval. wifi still powers down between attempts.
pub(crate) const RETRY_INTERVAL_SECS: u64 = 120;

// a wifi operation requested by the ui. each runs as one self-contained
// session on the wifi task: radio up, do the work, radio down.
pub(crate) enum Op {
    SyncTime,
    Scan,
    Join,
    Get {
        host: Host,
        path: String,
        max_body: usize,
    },
    Music {
        command: Option<&'static str>,
    },
    DownloadMaps {
        tiles: Vec<(u32, String)>,
        max_body: usize,
    },
}

// which server an `Op::Get` targets.
pub(crate) enum Host {
    // noot-server (music and environment json, and map tiles).
    Server,
    // an arbitrary public host over plain http on port 80 (weather api).
    External(&'static str),
}

// one queued operation plus the credentials to run it under.
pub(crate) struct Request {
    pub(crate) ssid: String,
    pub(crate) password: String,
    pub(crate) op: Op,
}

// a completed operation's result (or, for `Tile`, one step of an in-progress
// bulk download), consumed by the ui loop's event router.
pub(crate) enum Event {
    TimeSynced(Option<u64>),
    ScanDone(Option<Vec<ScanEntry>>),
    JoinDone(bool),
    GotBody(Option<Vec<u8>>),
    MusicDone(Option<MusicSnapshot>),
    // one fetched tile of a bulk download, streamed as it arrives so only one
    // body is ever resident.
    Tile { key: u32, body: Vec<u8> },
    DownloadDone { fetched: usize },
}

// single-slot channels between the ui and the wifi task: the ui queues at most
// one operation at a time (gated on its pending state) and drains events every
// pass; the task blocks on the event send for tile bodies, bounding memory.
static REQUESTS: Channel<CriticalSectionRawMutex, Request, 1> = Channel::new();
static EVENTS: Channel<CriticalSectionRawMutex, Event, 1> = Channel::new();
// set by the ui to abandon an in-flight bulk download (e.g. to enter sleep).
static CANCEL: AtomicBool = AtomicBool::new(false);

// queue an operation for the wifi task; false when one is already queued.
pub(crate) fn send(req: Request) -> bool {
    REQUESTS.try_send(req).is_ok()
}

// take the next completion event without blocking.
pub(crate) fn poll_event() -> Option<Event> {
    EVENTS.try_receive().ok()
}

// wait for the next completion event. used where the ui deliberately blocks on
// the in-flight session (boot sync, pre-sleep drain).
pub(crate) async fn next_event() -> Event {
    EVENTS.receive().await
}

// ask the task to abandon an in-flight bulk download after the current tile.
pub(crate) fn cancel() {
    CANCEL.store(true, Ordering::Relaxed);
}

// the wifi task: sole owner of the WIFI peripheral, running one session per
// queued request so exactly one controller ever exists and the radio is
// dropped (powered down) between sessions. nothing else may touch WIFI.
#[embassy_executor::task]
pub(crate) async fn run() {
    loop {
        let req = REQUESTS.receive().await;
        CANCEL.store(false, Ordering::Relaxed);
        // sound: this task is the only code that steals WIFI, one session at a
        // time, and the previous session's controller has been dropped.
        let wifi = unsafe { esp_hal::peripherals::WIFI::steal() };
        let event = match req.op {
            Op::SyncTime => Event::TimeSynced(sync_time(wifi, &req.ssid, &req.password).await),
            Op::Scan => Event::ScanDone(scan(wifi).await),
            Op::Join => Event::JoinDone(try_connect(wifi, &req.ssid, &req.password).await),
            Op::Get {
                host,
                path,
                max_body,
            } => Event::GotBody(
                http_get(wifi, &req.ssid, &req.password, &host, &path, max_body).await,
            ),
            Op::Music { command } => {
                Event::MusicDone(music_session(wifi, &req.ssid, &req.password, command).await)
            }
            Op::DownloadMaps { tiles, max_body } => Event::DownloadDone {
                fetched: download_maps(wifi, &req.ssid, &req.password, &tiles, max_body).await,
            },
        };
        EVENTS.send(event).await;
    }
}

// connect to wifi, fetch the current unix time via SNTP, then power the radio
// back down. self-contained: it drives the network stack (`runner`) alongside
// the connect+query work via `select`, so when the query finishes everything
// here drops — and WifiController's Drop deinitialises wifi (radio off), which
// frees the 2.4 GHz band for gps and saves power. returns UTC unix seconds, or
// None on timeout. re-callable for periodic re-sync (steal WIFI again).
// build a station-mode controller for the given credentials. shared by every
// wifi session below; dropping the returned controller deinitialises wifi
// (radio off). `WifiController::new` starts the driver in station mode, so a
// scan or connect can follow immediately.
fn station_controller(
    wifi: esp_hal::peripherals::WIFI<'static>,
    ssid: &str,
    password: &str,
) -> Option<WifiController<'static>> {
    let station_config = Config::Station(
        StationConfig::default()
            .with_ssid(ssid)
            .with_password(password.into()),
    );
    WifiController::new(
        wifi,
        ControllerConfig::default().with_initial_config(station_config),
    )
    .ok()
}

// connect can report a transient failure (commonly AuthenticationExpired at low
// signal) on the first attempt, so retry a few times before giving up.
const CONNECT_ATTEMPTS: u8 = 4;

// bring the association up, retrying transient connect failures, then wait for
// a dhcp lease. must be polled alongside the network stack's `runner` (via the
// `select` in each caller). returns whether the stack came up.
async fn bring_up(controller: &mut WifiController<'static>, stack: Stack<'_>) -> bool {
    let mut connected = false;
    for attempt in 1..=CONNECT_ATTEMPTS {
        match controller.connect_async().await {
            Ok(_) => {
                connected = true;
                break;
            }
            Err(e) => {
                esp_println::println!("wifi: connect attempt {attempt} failed: {e:?}");
                if attempt < CONNECT_ATTEMPTS {
                    Timer::after(Duration::from_secs(2)).await;
                }
            }
        }
    }
    if !connected {
        return false;
    }
    if with_timeout(Duration::from_secs(15), stack.wait_config_up())
        .await
        .is_err()
    {
        esp_println::println!("wifi: dhcp/config-up timed out");
        return false;
    }
    esp_println::println!("wifi: connected");
    true
}

// one detected access point, surfaced to the wifi settings page's scan list.
pub(crate) struct ScanEntry {
    pub(crate) ssid: String,
    pub(crate) rssi: i8,
    pub(crate) secured: bool,
}

// bring wifi up in station mode, scan for nearby access points, then power the
// radio back down. named networks are deduplicated (keeping the strongest
// signal) and hidden/unnamed APs dropped. returns None on timeout or scan
// error.
async fn scan(wifi: esp_hal::peripherals::WIFI<'static>) -> Option<Vec<ScanEntry>> {
    let mut controller = station_controller(wifi, "", "")?;

    let config = ScanConfig::default().with_max(20);
    let results = with_timeout(Duration::from_secs(15), controller.scan_async(&config))
        .await
        .ok()?
        .ok()?;

    let mut entries: Vec<ScanEntry> = results
        .into_iter()
        .filter(|ap| !ap.ssid.is_empty())
        .map(|ap| ScanEntry {
            ssid: ap.ssid.as_str().into(),
            rssi: ap.signal_strength,
            secured: !matches!(ap.auth_method, None | Some(AuthenticationMethod::None)),
        })
        .collect();

    // strongest first, then drop duplicate SSIDs (multiple bands/APs).
    entries.sort_by_key(|e| core::cmp::Reverse(e.rssi));
    let mut seen: Vec<String> = Vec::new();
    entries.retain(|e| {
        if seen.iter().any(|s| s == &e.ssid) {
            false
        } else {
            seen.push(e.ssid.clone());
            true
        }
    });
    Some(entries)
}

// bring wifi up with the given credentials and confirm a connection + DHCP
// lease come up, then power the radio back down. used by the wifi settings page
// to verify newly entered credentials. returns whether the join succeeded.
async fn try_connect(
    wifi: esp_hal::peripherals::WIFI<'static>,
    ssid: &str,
    password: &str,
) -> bool {
    esp_println::println!("wifi: try_connect ssid={ssid:?} pw_len={}", password.len());
    let Some(mut controller) = station_controller(wifi, ssid, password) else {
        esp_println::println!("wifi: controller init failed");
        return false;
    };

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
        bring_up(&mut controller, stack).await.then_some(())
    })
    .await;

    matches!(outcome, Either::Second(Some(())))
}

async fn sync_time(
    wifi: esp_hal::peripherals::WIFI<'static>,
    ssid: &str,
    password: &str,
) -> Option<u64> {
    esp_println::println!("wifi: sync_time ssid={ssid:?} pw_len={}", password.len());
    let mut controller = station_controller(wifi, ssid, password)?;

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
        if !bring_up(&mut controller, stack).await {
            return None;
        }
        for _ in 0..3 {
            if let Some(unix) = sntp_unix_time(stack).await {
                return Some(unix);
            }
            esp_println::println!("wifi: ntp query failed, retrying");
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

// bring wifi up, GET `path` from `host`, then power the radio back down.
// mirrors `sync_time`: it drives the network stack alongside the request work
// via `select`, so everything drops when the request finishes and
// WifiController's Drop deinitialises the radio. returns the response body, or
// None on any failure. `max_body` caps how much body is buffered: a few kB for
// json, much larger for a map tile.
async fn http_get(
    wifi: esp_hal::peripherals::WIFI<'static>,
    ssid: &str,
    password: &str,
    host: &Host,
    path: &str,
    max_body: usize,
) -> Option<Vec<u8>> {
    let mut controller = station_controller(wifi, ssid, password)?;

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
        if !bring_up(&mut controller, stack).await {
            return None;
        }
        match host {
            Host::Server => request(stack, "GET", path, max_body).await,
            Host::External(h) => request_host(stack, h, 80, path, max_body).await,
        }
    })
    .await;

    match outcome {
        Either::First(_) => None,
        Either::Second(body) => body,
    }
}

// consecutive failed tile fetches before a bulk download gives up (the network
// has clearly gone away, so don't eat the socket timeout for every remaining
// tile).
const MAX_CONSECUTIVE_MISSES: usize = 5;

// GET each map path from noot-server in one session, then power
// the radio back down. doing a whole area in one bring-up (rather than one per
// tile) keeps a bulk download to a single ~20s radio start. each fetched tile
// is streamed to the ui as an `Event::Tile` so it can be written to the sd
// cache as it arrives, keeping only one tile body resident at a time. stops
// early after a few consecutive misses or when cancelled. returns the number
// of tiles fetched.
async fn download_maps(
    wifi: esp_hal::peripherals::WIFI<'static>,
    ssid: &str,
    password: &str,
    tiles: &[(u32, String)],
    max_body: usize,
) -> usize {
    let Some(mut controller) = station_controller(wifi, ssid, password) else {
        return 0;
    };

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
        if !bring_up(&mut controller, stack).await {
            return None;
        }
        let mut fetched = 0usize;
        let mut misses = 0usize;
        for (key, path) in tiles {
            if CANCEL.load(Ordering::Relaxed) {
                esp_println::println!("wifi: bulk download cancelled");
                break;
            }
            match request(stack, "GET", path, max_body).await {
                Some(body) => {
                    misses = 0;
                    fetched += 1;
                    EVENTS.send(Event::Tile { key: *key, body }).await;
                }
                None => {
                    misses += 1;
                    if misses >= MAX_CONSECUTIVE_MISSES {
                        esp_println::println!(
                            "wifi: bulk download aborted after {misses} consecutive misses"
                        );
                        break;
                    }
                }
            }
        }
        Some(fetched)
    })
    .await;

    match outcome {
        Either::First(_) => 0,
        Either::Second(n) => n.unwrap_or(0),
    }
}

// the now-playing json plus the raw album-art bytes, fetched together in one
// wifi session so opening the music page (or hitting a control) costs a single
// radio bring-up.
pub(crate) struct MusicSnapshot {
    pub(crate) json: Vec<u8>,
    pub(crate) cover: Option<Vec<u8>>,
}

// upper bound on the album-art body we'll buffer (raw jpeg/png). covers from
// the backends are well under this; anything larger is dropped rather than
// risking the heap.
const MAX_COVER_BYTES: usize = 512 * 1024;

// bring wifi up, optionally POST a transport `command` (play-pause/next/etc.),
// then fetch the current now-playing json and album art, then power the radio
// back down. doing it all in one session keeps the music page to a single wifi
// bring-up per refresh. returns None if wifi never came up.
async fn music_session(
    wifi: esp_hal::peripherals::WIFI<'static>,
    ssid: &str,
    password: &str,
    command: Option<&str>,
) -> Option<MusicSnapshot> {
    let mut controller = station_controller(wifi, ssid, password)?;

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
        if !bring_up(&mut controller, stack).await {
            return None;
        }
        // best-effort: a failed control still lets us refresh the display.
        if let Some(command) = command {
            request(stack, "POST", command, 256).await;
            // give the backend a moment to apply the command before reading
            // state back, so the now-playing json and the cover reflect the same
            // (new) track rather than racing the backend's transition.
            Timer::after(Duration::from_millis(800)).await;
        }
        let json = request(stack, "GET", "/api/now-playing", 8192).await?;
        let cover = request(stack, "GET", "/api/now-playing/cover", MAX_COVER_BYTES).await;
        Some(MusicSnapshot { json, cover })
    })
    .await;

    match outcome {
        Either::First(_) => None,
        Either::Second(snapshot) => snapshot,
    }
}

// perform one HTTP request on an already-up stack and return the response body
// for a 2xx status (None otherwise, e.g. a 404 from the cover endpoint when the
// track has no art). `max_body` caps how much body we buffer.
async fn request(stack: Stack<'_>, method: &str, path: &str, max_body: usize) -> Option<Vec<u8>> {
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

    let mut req = String::new();
    write!(
        req,
        "{method} {path} HTTP/1.1\r\nHost: {SERVER_HOST}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    socket.write_all(req.as_bytes()).await.ok()?;

    read_response(&mut socket, max_body).await
}

// one plain-http GET to `host:port` (HTTP/1.0 so the body comes back
// connection-close rather than chunked, which this minimal read-until-close
// client can't decode), returning the response body for a 2xx status. `host` is
// used verbatim if it parses as an IPv4 address, otherwise resolved over DNS.
// `max_body` caps how much body is buffered.
async fn request_host(
    stack: Stack<'_>,
    host: &str,
    port: u16,
    path: &str,
    max_body: usize,
) -> Option<Vec<u8>> {
    let addr = match host.parse::<Ipv4Addr>() {
        Ok(ip) => IpAddress::Ipv4(ip),
        Err(_) => *stack.dns_query(host, DnsQueryType::A).await.ok()?.first()?,
    };

    let mut rx = [0u8; 1536];
    let mut tx = [0u8; 1536];
    let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
    socket.set_timeout(Some(Duration::from_secs(8)));
    socket.connect(IpEndpoint::new(addr, port)).await.ok()?;

    let mut req = String::new();
    write!(
        req,
        "GET {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    socket.write_all(req.as_bytes()).await.ok()?;

    read_response(&mut socket, max_body).await
}

// buffer a response until the server closes the connection, then strip the
// headers and return the body for a 2xx status. a body cut short by a mid-read
// socket error or the `max_body` cap returns None rather than a silently
// truncated buffer — callers cache these bodies (sd map tiles), so a partial
// body must never look like a success. when the server declares a
// content-length the body is validated against it; without one, only an
// orderly close marks the body complete.
async fn read_response(socket: &mut TcpSocket<'_>, max_body: usize) -> Option<Vec<u8>> {
    let mut resp = Vec::new();
    let mut chunk = [0u8; 1024];
    let mut closed = false;
    loop {
        match socket.read(&mut chunk).await {
            Ok(0) => {
                closed = true;
                break;
            }
            Ok(n) => {
                resp.extend_from_slice(&chunk[..n]);
                // headers + body; stop once we've buffered past the cap.
                if resp.len() > max_body + 2048 {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    // status line is "HTTP/1.x NNN ...": the first status digit sits at byte 9.
    if resp.get(9) != Some(&b'2') {
        return None;
    }
    let split = resp.windows(4).position(|w| w == b"\r\n\r\n")?;
    let body = &resp[split + 4..];
    match content_length(&resp[..split]) {
        Some(len) if body.len() >= len => Some(body[..len].to_vec()),
        Some(_) => None,
        None if closed => Some(body.to_vec()),
        None => None,
    }
}

// find a content-length header in the raw header block, if any.
fn content_length(headers: &[u8]) -> Option<usize> {
    headers.split(|&b| b == b'\n').find_map(|line| {
        let line = core::str::from_utf8(line).ok()?;
        let (name, value) = line.split_once(':')?;
        if name.trim().eq_ignore_ascii_case("content-length") {
            value.trim().parse::<usize>().ok()
        } else {
            None
        }
    })
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
    // a dropped reply must not hang the sync forever; the caller retries.
    let (n, _) = with_timeout(Duration::from_secs(5), socket.recv_from(&mut response))
        .await
        .ok()?
        .ok()?;
    if n < 44 {
        return None;
    }
    // transmit timestamp (seconds since 1900) is at bytes 40..44, big-endian.
    let ntp_secs = u32::from_be_bytes([response[40], response[41], response[42], response[43]]);
    Some((ntp_secs as u64).saturating_sub(NTP_UNIX_DELTA))
}
