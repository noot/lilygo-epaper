use core::{cell::RefCell, ops::Deref};

use esp_hal::{
    gpio::{Level, Output, OutputConfig},
    spi::master::Spi,
    Blocking,
};
use t5s3_epaper_core::{sdcard::Error, SdCard};

// an sd-card session: the mounted card plus the LoRa chip-select guard that
// must live exactly as long as it. the card shares SPI2 with the SX1262
// radio, which is dropped while off its screen; its chip-select floats, so it
// has to be held high for the whole session to make the idle radio release
// MISO (otherwise card init returns CardNotFound). bundling the guard with the
// card makes forgetting it impossible — always mount through `mount`, never
// `SdCard::new` directly.
pub(crate) struct Session<'a> {
    // declaration order is drop order: the card releases the bus before the
    // chip-select guard is let go.
    card: SdCard<'a, 'static>,
    _lora_cs: Output<'static>,
}

impl<'a> Deref for Session<'a> {
    type Target = SdCard<'a, 'static>;

    fn deref(&self) -> &Self::Target {
        &self.card
    }
}

// mount the sd card on the shared `bus`, holding the LoRa chip-select high
// for the life of the returned session.
pub(crate) fn mount<'a>(bus: &'a RefCell<Spi<'static, Blocking>>) -> Result<Session<'a>, Error> {
    // sound: GPIO46 belongs to the lora radio, which shares the single-owner
    // spi bus and so is never mid-transaction while a card session runs;
    // GPIO12 (card cs) is used by nothing else.
    let lora_cs = Output::new(
        unsafe { esp_hal::peripherals::GPIO46::steal() },
        Level::High,
        OutputConfig::default(),
    );
    let card = SdCard::new(unsafe { esp_hal::peripherals::GPIO12::steal() }, bus)?;
    Ok(Session {
        card,
        _lora_cs: lora_cs,
    })
}
