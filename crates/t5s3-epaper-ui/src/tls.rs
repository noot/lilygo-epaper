use core::sync::atomic::{AtomicU32, Ordering};

use embassy_net::tcp::TcpSocket;
use embedded_tls::{
    pki::CertVerifier,
    Aes128GcmSha256,
    Certificate,
    CryptoProvider,
    CryptoRngCore,
    TlsClock,
    TlsConfig,
    TlsConnection,
    TlsContext,
    TlsError,
    TlsVerifier,
};
use esp_hal::rng::Rng;

// let's encrypt's isrg root x1 (valid to 2035): the trust anchor that both
// api.open-meteo.com and tailscale funnel's *.ts.net certificates chain to.
static CA_ISRG_ROOT_X1: &[u8] = include_bytes!("../certs/isrg-root-x1.der");

// tls 1.3 record buffers: a record can be up to 16 KiB plus expansion.
pub(crate) const READ_BUF: usize = 16_640;
pub(crate) const WRITE_BUF: usize = 4_096;

// wall-clock unix seconds for certificate validity checks, kept fresh by the
// ui loop whenever its clock holds real time. while zero (clock never synced)
// the validity-window check is skipped; the chain and hostname are still
// verified.
static NOW_UNIX: AtomicU32 = AtomicU32::new(0);

pub(crate) fn set_now_unix(unix: u64) {
    NOW_UNIX.store(unix as u32, Ordering::Relaxed);
}

struct WallClock;

impl TlsClock for WallClock {
    fn now() -> Option<u64> {
        match NOW_UNIX.load(Ordering::Relaxed) {
            0 => None,
            now => Some(u64::from(now)),
        }
    }
}

// the s3's hardware rng, which is a true rng while the radio is running (and
// a tls session always runs inside a wifi session); esp-hal implements
// rand_core's RngCore for it but not the CryptoRng marker, so wrap it.
struct HwRng(Rng);

impl rand_core::RngCore for HwRng {
    fn next_u32(&mut self) -> u32 {
        self.0.random()
    }

    fn next_u64(&mut self) -> u64 {
        (u64::from(self.0.random()) << 32) | u64::from(self.0.random())
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for chunk in dest.chunks_mut(4) {
            let word = self.0.random().to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

impl rand_core::CryptoRng for HwRng {}

// a crypto provider that actually verifies the server: the certificate chain
// up to the pinned root, the hostname, and (clock permitting) the validity
// windows. uses embedded-tls's pure-rust pki verifier — the webpki one both
// needs `ring` (which doesn't build on xtensa) and can't walk intermediates.
struct Provider {
    rng: HwRng,
    verifier: CertVerifier<'static, Aes128GcmSha256, WallClock, 8192>,
}

impl CryptoProvider for Provider {
    type CipherSuite = Aes128GcmSha256;
    type Signature = &'static [u8];

    fn rng(&mut self) -> impl CryptoRngCore {
        &mut self.rng
    }

    fn verifier(&mut self) -> Result<&mut impl TlsVerifier<Self::CipherSuite>, TlsError> {
        Ok(&mut self.verifier)
    }
}

// a tls session presented as a plain byte stream: embedded-tls reports the
// server's close_notify as an error, but close_notify cryptographically marks
// the stream complete, so map it to a clean end-of-stream (read returning 0).
// the http layer relies on that to tell an orderly close from a truncated
// body.
pub(crate) struct Stream<'a> {
    inner: TlsConnection<'a, TcpSocket<'a>, Aes128GcmSha256>,
}

impl embedded_io_async::ErrorType for Stream<'_> {
    type Error = TlsError;
}

impl embedded_io_async::Read for Stream<'_> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        match self.inner.read(buf).await {
            Err(TlsError::ConnectionClosed) => Ok(0),
            other => other,
        }
    }
}

impl embedded_io_async::Write for Stream<'_> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.inner.write(buf).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush().await
    }
}

// open a verified tls 1.3 session over `socket` for `host`. the caller owns
// the record buffers so their (large) allocations stay visible at the call
// site.
pub(crate) async fn open<'a>(
    socket: TcpSocket<'a>,
    host: &str,
    read_buf: &'a mut [u8],
    write_buf: &'a mut [u8],
) -> Result<Stream<'a>, TlsError> {
    let config = TlsConfig::new().with_server_name(host);
    let provider = Provider {
        rng: HwRng(Rng::new()),
        verifier: CertVerifier::new(Certificate::X509(CA_ISRG_ROOT_X1)),
    };
    let mut connection = TlsConnection::new(socket, read_buf, write_buf);
    connection.open(TlsContext::new(&config, provider)).await?;
    Ok(Stream { inner: connection })
}
