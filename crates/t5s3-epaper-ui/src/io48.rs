use t5s3_epaper_core::FrontLight;

use crate::settings::Io48Action;

// what the main loop must do after an io48 press is dispatched.
pub(crate) enum Outcome {
    // enter deep sleep (main breaks its loop)
    Sleep,
    // switch to the lora screen's receive tab
    OpenLoraRecv,
    // handled in place (or a no-op); nothing for the loop to do
    Done,
}

// dispatch the settings-selected io48 action. `brightness` is the last value
// set from the frontlight page, used to restore the light when toggling on.
pub(crate) fn dispatch(action: Io48Action, light: &mut FrontLight, brightness: u8) -> Outcome {
    match action {
        Io48Action::Sleep => Outcome::Sleep,
        Io48Action::Backlight => {
            // toggle the front light between off and its last set brightness,
            // without leaving the current screen.
            if light.brightness() > 0 {
                light.off();
            } else if brightness > 0 {
                light.set_brightness(brightness);
            } else {
                // never set this boot; use a mid-range level so the toggle
                // visibly does something.
                light.set_brightness(50);
            }
            Outcome::Done
        }
        Io48Action::LoraReceive => Outcome::OpenLoraRecv,
        Io48Action::Nothing => Outcome::Done,
    }
}
