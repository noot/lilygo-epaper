//! Wi-Fi soft-AP to LoRa bridge: the board hosts an open Wi-Fi network and a tiny
//! web page. Join the network from a phone, open `http://192.168.4.1/`, type a
//! message, hit send, and it goes out over LoRa (SX1262). The last message sent
//! is shown on the e-paper display.
//!
//! esp-radio is the Wi-Fi driver; this example owns the network bring-up: it runs
//! a soft access point, a minimal DHCP server (so the phone gets an address) and a
//! small HTTP server, all driven through `smoltcp` in a blocking poll loop.
//!
//! Flash with `cargo run --example wifi_tx` (requires the `esp` toolchain + espflash).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec;
use core::fmt::Write as _;

use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::{FONT_6X10, FONT_10X20};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Line, PrimitiveStyle};
use embedded_graphics::text::Text;
use embedded_hal::delay::DelayNs as _;
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::main;
use esp_hal::spi::Mode;
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::time::{Instant as HalInstant, Rate};
use esp_hal::timer::timg::TimerGroup;
use esp_println::println;
use esp_radio::wifi::ap::AccessPointConfig;
use esp_radio::wifi::{
    Config as WifiConfig, ControllerConfig, Interface, WifiRxToken, WifiTxToken,
};
use smoltcp::iface::{Config as IfaceConfig, Interface as NetInterface, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::{tcp, udp};
use smoltcp::time::Instant as NetInstant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, IpEndpoint};

use lilygo_t3s3_epaper::ssd1680::{Display, Rotation};
use lilygo_t3s3_epaper::sx1262::{Config as RadioConfig, Sx1262};

esp_bootloader_esp_idf::esp_app_desc!();

/// SSID of the open access point the phone joins.
const SSID: &str = "lora-tx";
/// Gateway / server address handed out by DHCP and served by the HTTP server.
const GATEWAY: [u8; 4] = [192, 168, 4, 1];
/// The single address leased to the connecting phone.
const CLIENT_IP: [u8; 4] = [192, 168, 4, 2];
/// esp-radio's default Wi-Fi MTU; advertised to smoltcp so it never hands the TX
/// token a frame longer than the driver's internal buffer.
const MTU: usize = 1492;
/// Number of HTTP listener sockets — phones open several connections in parallel.
const HTTP_SOCKETS: usize = 3;
/// Longest LoRa payload we accept from the form.
const MSG_CAP: usize = 200;

#[main]
fn main() -> ! {
    // 160 MHz is the one clock that suits both radios on this board: 240 MHz
    // mis-samples the SX1262's MISO reads, and 80 MHz is the floor esp-radio needs
    // for Wi-Fi. See the LoRa + radio-coexistence notes.
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::_160MHz));

    // the Wi-Fi controller firmware needs a heap and a preemptive scheduler.
    esp_alloc::heap_allocator!(size: 115 * 1024);
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_ints = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_ints.software_interrupt0);

    // lora radio (sx1262) on its own spi bus: sck=5, mosi=6, miso=3, nss=7.
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
    // power the radio's oscillator rail (gpio35); hold the handle so it stays
    // driven for the life of the program, else the xosc never starts.
    let _radio_pow = Output::new(peripherals.GPIO35, Level::High, OutputConfig::default());
    Delay::new().delay_ms(10);
    let mut radio = Sx1262::new(
        radio_spi,
        radio_cs,
        radio_rst,
        radio_busy,
        radio_dio1,
        Delay::new(),
        RadioConfig::default(),
    );
    radio.init().unwrap();
    println!(
        "sx1262 ready at 915 MHz (status={:#04x}, device_errors={:#06x})",
        radio.status().unwrap(),
        radio.device_errors().unwrap()
    );

    // e-paper display on a second spi bus: sclk=14, mosi=11, cs=15.
    let disp_spi = Spi::new(
        peripherals.SPI3,
        SpiConfig::default()
            .with_frequency(Rate::from_mhz(4))
            .with_mode(Mode::_0),
    )
    .unwrap()
    .with_sck(peripherals.GPIO14)
    .with_mosi(peripherals.GPIO11);
    let disp_cs = Output::new(peripherals.GPIO15, Level::High, OutputConfig::default());
    let disp_dev = ExclusiveDevice::new(disp_spi, disp_cs, Delay::new()).unwrap();
    let disp_dc = Output::new(peripherals.GPIO16, Level::Low, OutputConfig::default());
    let disp_rst = Output::new(peripherals.GPIO47, Level::High, OutputConfig::default());
    let disp_busy = Input::new(
        peripherals.GPIO48,
        InputConfig::default().with_pull(Pull::None),
    );
    let mut display = Display::new(disp_dev, disp_dc, disp_rst, disp_busy, Delay::new());
    display.set_rotation(Rotation::Rotate270); // landscape, 250 x 122
    display.init().unwrap();

    // bring up the open soft access point. set_config also starts the AP.
    let (mut controller, interfaces) =
        esp_radio::wifi::new(peripherals.WIFI, ControllerConfig::default()).unwrap();
    let ap_config = AccessPointConfig::default().with_ssid(SSID);
    controller
        .set_config(&WifiConfig::AccessPoint(ap_config))
        .unwrap();
    Delay::new().delay_ms(100);
    println!("soft-AP '{SSID}' up; join it and open http://192.168.4.1/");

    render(
        &mut display,
        "WiFi -> LoRa",
        "join wifi: lora-tx",
        "open http://192.168.4.1",
        "waiting for a message",
    );
    display.refresh().unwrap();

    // smoltcp network stack over the access-point interface.
    let mut device = WifiStackDevice {
        iface: interfaces.access_point,
    };
    let mac = device.iface.mac_address();
    let mut iface_config = IfaceConfig::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
    iface_config.random_seed = HalInstant::now().duration_since_epoch().as_micros();
    let mut iface = NetInterface::new(iface_config, &mut device, now());
    iface.update_ip_addrs(|addrs| {
        let _ = addrs.push(IpCidr::new(
            IpAddress::v4(GATEWAY[0], GATEWAY[1], GATEWAY[2], GATEWAY[3]),
            24,
        ));
    });

    let mut sockets = SocketSet::new(vec![]);
    let dhcp_handle = {
        let rx = udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 8], vec![0u8; 1024]);
        let tx = udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 8], vec![0u8; 1024]);
        let mut socket = udp::Socket::new(rx, tx);
        socket.bind(67).unwrap();
        sockets.add(socket)
    };
    // captive-portal DNS: answer every query with our own address so the phone's
    // connectivity check reaches our HTTP server and pops the sign-in page.
    let dns_handle = {
        let rx = udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 8], vec![0u8; 1024]);
        let tx = udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 8], vec![0u8; 1024]);
        let mut socket = udp::Socket::new(rx, tx);
        socket.bind(53).unwrap();
        sockets.add(socket)
    };
    let http_handles: [_; HTTP_SOCKETS] = core::array::from_fn(|_| {
        let rx = tcp::SocketBuffer::new(vec![0u8; 1536]);
        let tx = tcp::SocketBuffer::new(vec![0u8; 2560]);
        sockets.add(tcp::Socket::new(rx, tx))
    });

    let mut delay = Delay::new();
    let mut counter: u32 = 0;
    loop {
        iface.poll(now(), &mut device, &mut sockets);

        // hand the connecting phone an address.
        let dhcp = sockets.get_mut::<udp::Socket>(dhcp_handle);
        if dhcp.can_recv() {
            serve_dhcp(dhcp);
        }

        // point every name at us so the captive-portal check lands on our page.
        let dns = sockets.get_mut::<udp::Socket>(dns_handle);
        if dns.can_recv() {
            serve_dns(dns);
        }

        // serve the form and pick up any message the phone submitted.
        let mut submitted: Option<([u8; MSG_CAP], usize)> = None;
        for &handle in &http_handles {
            let socket = sockets.get_mut::<tcp::Socket>(handle);
            if !socket.is_open() {
                let _ = socket.listen(80);
            }
            if !socket.can_recv() {
                continue;
            }

            let mut request = [0u8; 1024];
            let n = socket.recv_slice(&mut request).unwrap_or(0);
            let mut msg = [0u8; MSG_CAP];
            let msg_len = parse_send(&request[..n], &mut msg).unwrap_or(0);

            let status = if msg_len > 0 {
                match radio.transmit(&msg[..msg_len]) {
                    Ok(()) => {
                        println!("tx #{counter}: {:?}", core::str::from_utf8(&msg[..msg_len]));
                        submitted = Some((msg, msg_len));
                        counter = counter.wrapping_add(1);
                        Status::Sent(&msg[..msg_len])
                    }
                    Err(e) => {
                        println!("tx error: {e:?}");
                        Status::Error
                    }
                }
            } else {
                Status::None
            };

            let mut page = PageBuf::new();
            write_page(&mut page, status);
            let _ = socket.send_slice(page.as_bytes());
            socket.close();
        }

        if let Some((msg, len)) = submitted {
            let mut line = FmtBuf::new();
            match core::str::from_utf8(&msg[..len]) {
                Ok(text) => {
                    let _ = write!(line, "sent: {text}");
                }
                Err(_) => {
                    let _ = write!(line, "sent: <binary>");
                }
            }
            let mut count = FmtBuf::new();
            let _ = write!(count, "#{}", counter.wrapping_sub(1));
            render(
                &mut display,
                "WiFi -> LoRa",
                "join wifi: lora-tx",
                count.as_str(),
                line.as_str(),
            );
            display.refresh_partial().unwrap();
        }

        delay.delay_ms(5);
    }
}

/// What the served page should report about the last action.
enum Status<'a> {
    None,
    Sent(&'a [u8]),
    Error,
}

/// Build the HTTP response: a small form, plus a status line after a submission.
fn write_page(out: &mut PageBuf, status: Status<'_>) {
    let _ = out.write_str(
        "HTTP/1.0 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n\
         <!doctype html><html><head><meta charset=utf-8>\
         <meta name=viewport content=\"width=device-width,initial-scale=1\">\
         <title>LoRa TX</title></head><body style=\"font-family:sans-serif;margin:2em\">\
         <h2>Send a LoRa message</h2>\
         <form action=\"/send\" method=\"get\">\
         <input name=\"msg\" autofocus autocomplete=off maxlength=200 \
         style=\"font-size:1.2em;width:70%\">\
         <button type=submit style=\"font-size:1.2em\">Send</button></form>",
    );
    match status {
        Status::None => {}
        Status::Sent(msg) => {
            let _ = out.write_str("<p style=\"color:green\">Sent: ");
            out.write_escaped(msg);
            let _ = out.write_str("</p>");
        }
        Status::Error => {
            let _ = out.write_str("<p style=\"color:red\">Send failed — try again.</p>");
        }
    }
    let _ = out.write_str("</body></html>");
}

/// Parse `GET /send?msg=...` from an HTTP request, writing the URL-decoded message
/// into `out`. Returns the decoded length, or `None` if this is not a send request.
fn parse_send(request: &[u8], out: &mut [u8]) -> Option<usize> {
    let line_end = request.iter().position(|&b| b == b'\r' || b == b'\n')?;
    let path = request[..line_end].strip_prefix(b"GET ")?;
    let path_end = path.iter().position(|&b| b == b' ')?;
    let query = path[..path_end].strip_prefix(b"/send?")?;
    for param in query.split(|&b| b == b'&') {
        if let Some(value) = param.strip_prefix(b"msg=") {
            return Some(url_decode(value, out));
        }
    }
    None
}

/// Decode an `application/x-www-form-urlencoded` value into `out`, returning its
/// length (capped at `out.len()`).
fn url_decode(input: &[u8], out: &mut [u8]) -> usize {
    let mut written = 0;
    let mut i = 0;
    while i < input.len() && written < out.len() {
        match input[i] {
            b'+' => {
                out[written] = b' ';
                i += 1;
            }
            b'%' if i + 2 < input.len() => {
                match (hex_value(input[i + 1]), hex_value(input[i + 2])) {
                    (Some(hi), Some(lo)) => {
                        out[written] = (hi << 4) | lo;
                        i += 3;
                    }
                    _ => {
                        out[written] = b'%';
                        i += 1;
                    }
                }
            }
            byte => {
                out[written] = byte;
                i += 1;
            }
        }
        written += 1;
    }
    written
}

fn hex_value(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Minimal DHCP server: answer a DISCOVER with an OFFER and a REQUEST with an ACK,
/// always leasing [`CLIENT_IP`]. Enough for one phone to get on the network.
fn serve_dhcp(socket: &mut udp::Socket) {
    let mut request = [0u8; 600];
    let len = match socket.recv_slice(&mut request) {
        Ok((len, _)) => len,
        Err(_) => return,
    };
    // need the fixed BOOTP header plus the 4-byte DHCP magic cookie.
    if len < 240 || request[236..240] != [0x63, 0x82, 0x53, 0x63] {
        return;
    }
    let reply_type = match find_option(&request[240..len], 53).and_then(|v| v.first().copied()) {
        Some(1) => 2, // DISCOVER -> OFFER
        Some(3) => 5, // REQUEST  -> ACK
        _ => return,
    };

    let mut reply = [0u8; 300];
    reply[0] = 2; // BOOTREPLY
    reply[1] = 1; // htype: ethernet
    reply[2] = 6; // hlen
    reply[4..8].copy_from_slice(&request[4..8]); // xid
    reply[10..12].copy_from_slice(&request[10..12]); // flags
    reply[16..20].copy_from_slice(&CLIENT_IP); // yiaddr
    reply[20..24].copy_from_slice(&GATEWAY); // siaddr
    reply[28..44].copy_from_slice(&request[28..44]); // chaddr
    reply[236..240].copy_from_slice(&[0x63, 0x82, 0x53, 0x63]); // magic cookie

    let mut at = 240;
    let mut option = |buf: &mut [u8; 300], code: u8, data: &[u8]| {
        buf[at] = code;
        buf[at + 1] = data.len() as u8;
        buf[at + 2..at + 2 + data.len()].copy_from_slice(data);
        at += 2 + data.len();
    };
    option(&mut reply, 53, &[reply_type]);
    option(&mut reply, 54, &GATEWAY); // server identifier
    option(&mut reply, 51, &86_400u32.to_be_bytes()); // lease time
    option(&mut reply, 1, &[255, 255, 255, 0]); // subnet mask
    option(&mut reply, 3, &GATEWAY); // router
    option(&mut reply, 6, &GATEWAY); // dns
    reply[at] = 255; // end
    at += 1;

    // the client has no address yet, so reply to the broadcast address.
    let to = IpEndpoint::new(IpAddress::v4(255, 255, 255, 255), 68);
    let _ = socket.send_slice(&reply[..at], to);
}

/// Minimal captive-portal DNS server: answer every A query with [`GATEWAY`], so
/// the phone's connectivity check resolves to us and hits the HTTP server. Other
/// query types get an empty (no-error) answer so the client falls back to A.
fn serve_dns(socket: &mut udp::Socket) {
    let mut query = [0u8; 512];
    let (len, meta) = match socket.recv_slice(&mut query) {
        Ok(value) => value,
        Err(_) => return,
    };
    // need at least the 12-byte header and one question.
    if len < 13 || u16::from_be_bytes([query[4], query[5]]) < 1 {
        return;
    }
    // walk the QNAME labels to find where QTYPE/QCLASS sit.
    let mut at = 12;
    while at < len && query[at] != 0 {
        let label = query[at] as usize;
        if label & 0xC0 != 0 {
            return; // compressed name in a question: bail out.
        }
        at += label + 1;
    }
    let question_end = at + 5; // null label (1) + qtype (2) + qclass (2)
    if question_end > len {
        return;
    }
    let is_a_query = u16::from_be_bytes([query[at + 1], query[at + 2]]) == 1;

    let mut reply = [0u8; 512];
    reply[..question_end].copy_from_slice(&query[..question_end]);
    reply[2] = 0x81; // QR=1, opcode=0, RD copied
    reply[3] = 0x80; // RA=1, rcode=0
    reply[4..6].copy_from_slice(&1u16.to_be_bytes()); // qdcount
    reply[6..8].copy_from_slice(&u16::from(is_a_query).to_be_bytes()); // ancount
    reply[8..12].copy_from_slice(&[0, 0, 0, 0]); // nscount / arcount

    let mut at = question_end;
    if is_a_query {
        reply[at..at + 2].copy_from_slice(&[0xC0, 0x0C]); // name -> offset 12
        reply[at + 2..at + 4].copy_from_slice(&1u16.to_be_bytes()); // type A
        reply[at + 4..at + 6].copy_from_slice(&1u16.to_be_bytes()); // class IN
        reply[at + 6..at + 10].copy_from_slice(&60u32.to_be_bytes()); // TTL
        reply[at + 10..at + 12].copy_from_slice(&4u16.to_be_bytes()); // rdlength
        reply[at + 12..at + 16].copy_from_slice(&GATEWAY); // rdata
        at += 16;
    }
    let _ = socket.send_slice(&reply[..at], meta.endpoint);
}

/// Find a DHCP option's value by code within the option TLV region.
fn find_option(options: &[u8], code: u8) -> Option<&[u8]> {
    let mut i = 0;
    while i < options.len() {
        match options[i] {
            255 => break, // end
            0 => i += 1,  // pad
            c => {
                let len = *options.get(i + 1)? as usize;
                let start = i + 2;
                let end = start + len;
                if end > options.len() {
                    break;
                }
                if c == code {
                    return Some(&options[start..end]);
                }
                i = end;
            }
        }
    }
    None
}

/// The current monotonic time as a smoltcp [`NetInstant`].
fn now() -> NetInstant {
    NetInstant::from_micros(HalInstant::now().duration_since_epoch().as_micros() as i64)
}

/// smoltcp [`Device`] over esp-radio's Wi-Fi [`Interface`]; the RX/TX tokens just
/// forward to the driver's own buffer-borrowing tokens.
struct WifiStackDevice<'d> {
    iface: Interface<'d>,
}

struct DeviceRxToken(WifiRxToken);
struct DeviceTxToken(WifiTxToken);

impl Device for WifiStackDevice<'_> {
    type RxToken<'a>
        = DeviceRxToken
    where
        Self: 'a;
    type TxToken<'a>
        = DeviceTxToken
    where
        Self: 'a;

    fn receive(
        &mut self,
        _timestamp: NetInstant,
    ) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        self.iface
            .receive()
            .map(|(rx, tx)| (DeviceRxToken(rx), DeviceTxToken(tx)))
    }

    fn transmit(&mut self, _timestamp: NetInstant) -> Option<Self::TxToken<'_>> {
        self.iface.transmit().map(DeviceTxToken)
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = MTU;
        caps
    }
}

impl RxToken for DeviceRxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        self.0.consume_token(|buf| f(buf))
    }
}

impl TxToken for DeviceTxToken {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        self.0.consume_token(len, f)
    }
}

/// draw a title, a rule and up to three body lines into the framebuffer.
fn render<D>(display: &mut D, title: &str, line1: &str, line2: &str, line3: &str)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let title_style = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
    let body = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
    let rule = PrimitiveStyle::with_stroke(BinaryColor::On, 1);

    let _ = display.clear(BinaryColor::Off);
    let _ = Text::new(title, Point::new(8, 24), title_style).draw(display);
    let _ = Line::new(Point::new(8, 32), Point::new(242, 32))
        .into_styled(rule)
        .draw(display);
    let _ = Text::new(line1, Point::new(8, 52), body).draw(display);
    let _ = Text::new(line2, Point::new(8, 68), body).draw(display);
    let _ = Text::new(line3, Point::new(8, 84), body).draw(display);
}

/// a fixed-capacity buffer for building the HTTP response, with helpers to append
/// HTML-escaped bytes from the (untrusted) submitted message.
struct PageBuf {
    buf: [u8; 1536],
    len: usize,
}

impl PageBuf {
    fn new() -> Self {
        Self {
            buf: [0; 1536],
            len: 0,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    fn push(&mut self, s: &str) {
        let bytes = s.as_bytes();
        let n = bytes.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&bytes[..n]);
        self.len += n;
    }

    /// append `bytes` with the five HTML-significant characters escaped.
    fn write_escaped(&mut self, bytes: &[u8]) {
        for &b in bytes {
            match b {
                b'&' => self.push("&amp;"),
                b'<' => self.push("&lt;"),
                b'>' => self.push("&gt;"),
                b'"' => self.push("&quot;"),
                b'\'' => self.push("&#39;"),
                _ => {
                    if self.len < self.buf.len() {
                        self.buf[self.len] = b;
                        self.len += 1;
                    }
                }
            }
        }
    }
}

impl core::fmt::Write for PageBuf {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.push(s);
        Ok(())
    }
}

/// a tiny fixed-capacity buffer that implements `core::fmt::Write` for display lines.
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
