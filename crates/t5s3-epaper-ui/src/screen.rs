#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Screen {
    Home,
    Gps,
    Lora,
    Frontlight,
    Sleep,
    Info,
    Files,
    Image,
    Reader,
    Settings,
    Music,
    Environment,
    Library,
    Weather,
    SettingsSystem,
    SettingsReader,
    SettingsWifi,
}

impl Screen {
    pub(crate) fn to_index(self) -> u8 {
        match self {
            Screen::Home => 0,
            Screen::Gps => 1,
            Screen::Lora => 2,
            Screen::Frontlight => 3,
            Screen::Sleep => 4,
            Screen::Info => 5,
            Screen::Files => 6,
            Screen::Image => 7,
            Screen::Reader => 8,
            Screen::Settings => 9,
            Screen::Music => 10,
            Screen::Environment => 11,
            Screen::Library => 12,
            Screen::Weather => 13,
            Screen::SettingsSystem => 14,
            Screen::SettingsReader => 15,
            Screen::SettingsWifi => 16,
        }
    }

    // map a stored index back to a screen. the Sleep screen and any
    // unexpected value fall back to Home, so waking never lands on the sleep
    // menu or on garbage left by an interrupted persistent write.
    pub(crate) fn from_index(value: u8) -> Self {
        match value {
            1 => Screen::Gps,
            2 => Screen::Lora,
            3 => Screen::Frontlight,
            5 => Screen::Info,
            6 => Screen::Files,
            9 => Screen::Settings,
            10 => Screen::Music,
            11 => Screen::Environment,
            12 => Screen::Library,
            13 => Screen::Weather,
            14 => Screen::SettingsSystem,
            15 => Screen::SettingsReader,
            16 => Screen::SettingsWifi,
            _ => Screen::Home,
        }
    }
}
