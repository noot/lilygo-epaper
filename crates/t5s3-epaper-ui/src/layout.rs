use t5s3_epaper_core::Display;

pub(crate) const SCREEN_W: i32 = 540;
pub(crate) const STATUS_H: i32 = 55;

// margin from the physical screen edges that the case's bezel overlaps;
// rounded-corner touch targets (the keyboard) need to stay inside this or
// their corners get clipped. about the same margin as the notes editor's
// text box (20px each side), a few pixels more generous.
pub(crate) const SAFE_MARGIN: i32 = 15;
pub(crate) const SAFE_X: i32 = SAFE_MARGIN;
pub(crate) const SAFE_W: i32 = SCREEN_W - 2 * SAFE_MARGIN;

// Rotate270: screen(x,y) → native(y, 539-x)
// Inverse for touch: screen_x = 539 - native_y, screen_y = native_x
pub(crate) fn touch_to_screen(tx: u16, ty: u16) -> (i32, i32) {
    (539 - ty as i32, tx as i32)
}

pub(crate) fn screen_to_native_rect(
    sx: i32,
    sy: i32,
    sw: i32,
    sh: i32,
) -> t5s3_epaper_core::display::Rectangle {
    t5s3_epaper_core::display::Rectangle {
        x: sy as u16,
        y: (Display::HEIGHT as i32 - sx - sw) as u16,
        width: sh as u16,
        height: sw as u16,
    }
}
