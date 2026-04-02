use core::time::Duration;

use esp_hal::{peripherals::LPWR, rtc_cntl::Rtc};

/// Lightweight wrapper around the ESP32-S3 RTC timekeeping functions.
pub struct Clock<'d> {
    lpwr: LPWR<'d>,
}

impl<'d> Clock<'d> {
    /// Create a new RTC clock wrapper.
    pub fn new(lpwr: LPWR<'d>) -> Self {
        Self { lpwr }
    }

    fn rtc(&mut self) -> Rtc<'_> {
        Rtc::new(self.lpwr.reborrow())
    }

    /// Return the current RTC-backed wall-clock time in microseconds.
    pub fn now_us(&mut self) -> u64 {
        self.rtc().current_time_us()
    }

    /// Return the current RTC-backed wall-clock time as a duration.
    pub fn now(&mut self) -> Duration {
        Duration::from_micros(self.now_us())
    }

    /// Set the current RTC-backed wall-clock time in microseconds.
    pub fn set_now_us(&mut self, now_us: u64) {
        self.rtc().set_current_time_us(now_us);
    }

    /// Set the current RTC-backed wall-clock time.
    pub fn set_now(&mut self, now: Duration) {
        self.set_now_us(now.as_micros() as u64);
    }

    /// Return the time since boot.
    pub fn uptime(&mut self) -> Duration {
        Duration::from_micros(self.rtc().time_since_boot().as_micros())
    }

    /// Return the estimated XTAL frequency in MHz.
    pub fn estimate_xtal_frequency_mhz(&mut self) -> u32 {
        self.rtc().estimate_xtal_frequency()
    }

    /// Return the owned LPWR peripheral for use by other low-power APIs.
    pub fn into_inner(self) -> LPWR<'d> {
        self.lpwr
    }
}
