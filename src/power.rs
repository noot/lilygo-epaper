use core::time::Duration;

use esp_hal::{
    gpio::{AnyPin, RtcPin},
    peripherals::LPWR,
    rtc_cntl::{
        sleep::{Ext0WakeupSource, RtcSleepConfig, TimerWakeupSource, WakeupLevel},
        Rtc, SocResetReason,
    },
    system::{reset_reason, wakeup_cause, SleepSource},
};

/// Reset and wake information for the current boot.
#[derive(Clone, Copy, Debug)]
pub struct WakeStatus {
    pub reset_reason: Option<SocResetReason>,
    pub wakeup_cause: SleepSource,
}

impl WakeStatus {
    /// True when the chip resumed from deep sleep.
    pub fn woke_from_deep_sleep(&self) -> bool {
        self.reset_reason == Some(SocResetReason::CoreDeepSleep)
    }

    /// True when the boot button woke the chip from deep sleep.
    pub fn woke_from_boot_button(&self) -> bool {
        self.woke_from_deep_sleep() && matches!(self.wakeup_cause, SleepSource::Ext0)
    }
}

/// Read the current reset and wakeup reason.
pub fn wake_status() -> WakeStatus {
    WakeStatus {
        reset_reason: reset_reason(),
        wakeup_cause: wakeup_cause(),
    }
}

/// Enter deep sleep and wake either from the boot button or an optional timer.
pub fn deep_sleep(lpwr: LPWR<'_>, mut boot_button: AnyPin<'_>, timer: Option<Duration>) -> ! {
    boot_button.rtcio_pad_hold(false);

    let mut rtc = Rtc::new(lpwr);
    let mut rtc_cfg = RtcSleepConfig::deep();
    rtc_cfg.set_rtc_fastmem_pd_en(false);
    rtc_cfg.set_rtc_slowmem_pd_en(false);

    let ext0 = Ext0WakeupSource::new(boot_button.reborrow(), WakeupLevel::Low);

    match timer {
        Some(duration) => {
            let timer = TimerWakeupSource::new(duration);
            rtc.sleep(&rtc_cfg, &[&timer, &ext0]);
        }
        None => rtc.sleep(&rtc_cfg, &[&ext0]),
    }

    loop {
        core::hint::spin_loop();
    }
}
