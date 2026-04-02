use crate::touchscreen::TouchState;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Buttons {
    pub home: bool,
    pub auxiliary: bool,
    pub boot: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputState {
    pub touch: Option<TouchState>,
    pub buttons: Buttons,
}
