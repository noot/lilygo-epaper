//! Error type for the SSD1680 display driver.

/// An error returned by the SSD1680 driver.
///
/// Generic over the SPI device error `S` and the GPIO error `P` so the caller's
/// concrete `embedded-hal` error types are preserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error<S, P> {
    /// The SPI transfer failed.
    Spi(S),
    /// A GPIO operation failed.
    Pin(P),
    /// The panel did not report ready in time.
    Timeout,
}
