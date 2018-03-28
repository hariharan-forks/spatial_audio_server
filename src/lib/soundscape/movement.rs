use metres::Metres;
use nannou::prelude::*;

/// The bounding box for an iterator yielding points.
#[derive(Copy, Clone, Debug)]
pub struct BoundingBox {
    pub left: Metres,
    pub right: Metres,
    pub top: Metres,
    pub bottom: Metres,
}

impl BoundingBox {
    /// Initialise a bounding box at a single point in space.
    pub fn from_point(p: Point2<Metres>) -> Self {
        BoundingBox { left: p.x, right: p.x, top: p.y, bottom: p.y }
    }

    /// Determine the movement area bounds on the given set of points.
    pub fn from_points<I>(points: I) -> Option<Self>
    where
        I: IntoIterator<Item=Point2<Metres>>,
    {
        let mut points = points.into_iter();
        points
            .next()
            .map(|p| {
                let init = BoundingBox::from_point(p);
                points.fold(init, BoundingBox::with_point)
            })
    }

    /// Extend the bounding box to include the given point.
    pub fn with_point(self, p: Point2<Metres>) -> Self {
        BoundingBox {
            left: p.x.min(self.left),
            right: p.x.max(self.right),
            bottom: p.y.min(self.bottom),
            top: p.y.max(self.top),
        }
    }

    /// The middle of the bounding box.
    pub fn middle(&self) -> Point2<Metres> {
        Point2 {
            x: (self.left + self.right) * 0.5,
            y: (self.bottom + self.top) * 0.5,
        }
    }
}