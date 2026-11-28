use implicit_forge::scene::{Stamp, StampOp, StampShape};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Center(usize),
    Radius,
    Extent(usize),
    Height,
    Blend,
}

impl Field {
    pub fn dimensions(stamp: Stamp) -> &'static [Self] {
        use Field::*;
        let smooth = matches!(stamp.op, StampOp::Smooth | StampOp::SmoothSubtract);
        match (stamp.shape, smooth) {
            (StampShape::Box, false) => &[Extent(0), Extent(1), Extent(2)],
            (StampShape::Box, true) => &[Extent(0), Extent(1), Extent(2), Blend],
            (StampShape::Cylinder, false) => &[Radius, Height],
            (StampShape::Cylinder, true) => &[Radius, Height, Blend],
            (_, false) => &[Radius],
            (_, true) => &[Radius, Blend],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Center(i) => ["Center X", "Center Y", "Center Z"][i],
            Self::Extent(i) => ["Local width", "Local height", "Local depth"][i],
            Self::Radius => "Radius",
            Self::Height => "Height",
            Self::Blend => "Blend width",
        }
    }

    pub fn value(self, stamp: Stamp) -> f32 {
        match self {
            Self::Center(i) => stamp.center[i],
            Self::Extent(i) => stamp.half_extents[i] * 2.,
            Self::Radius => stamp.size,
            Self::Height => stamp.half_height * 2.,
            Self::Blend => stamp.blend,
        }
    }

    fn range(self) -> (f32, f32) {
        match self {
            Self::Center(_) => (-2., 2.),
            Self::Extent(_) | Self::Height => (0.10, 4.),
            Self::Radius => (0.15, 1.5),
            Self::Blend => (0., 1.),
        }
    }

    pub fn fraction(self, stamp: Stamp) -> f32 {
        let (min, max) = self.range();
        (self.value(stamp) - min) / (max - min)
    }

    pub fn set(self, stamp: &mut Stamp, fraction: f32) {
        let (min, max) = self.range();
        let value = min + (max - min) * fraction.clamp(0., 1.);
        match self {
            Self::Center(i) => stamp.center[i] = value,
            Self::Extent(i) => stamp.half_extents[i] = value * 0.5,
            Self::Radius => stamp.size = value,
            Self::Height => stamp.half_height = value * 0.5,
            Self::Blend => stamp.blend = value,
        }
    }
}
