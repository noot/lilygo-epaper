#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TouchPoint {
    pub id: u8,
    pub x: u16,
    pub y: u16,
    pub size: u16,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TouchState {
    pub count: u8,
    pub points: [TouchPoint; 5],
}

impl TouchState {
    pub fn first_point(&self) -> Option<TouchPoint> {
        if self.count == 0 {
            None
        } else {
            Some(self.points[0])
        }
    }
}
