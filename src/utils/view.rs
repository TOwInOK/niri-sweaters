//! Viewport transform in output-local logical coordinates.
//!
//! Maps between content and displayed positions within a single output:
//! `display = focal + (content - focal) * factor`.

use smithay::utils::{Logical, Point, Rectangle};

/// A uniform scale transform around a fixed focal point.
///
/// `focal` is the fixed point of the transform: `apply(focal) == focal`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewportTransform {
    focal: Point<f64, Logical>,
    factor: f64,
}

impl ViewportTransform {
    /// Creates a transform with the given focal point and scale factor.
    ///
    /// # Panics
    ///
    /// Panics if `factor` is not finite or not positive.
    pub fn new(focal: Point<f64, Logical>, factor: f64) -> Self {
        assert!(factor.is_finite() && factor > 0.);
        Self { focal, factor }
    }

    /// Returns the identity transform.
    pub fn identity() -> Self {
        Self::new(Point::from((0., 0.)), 1.)
    }

    /// The fixed point of this transform.
    pub fn focal(&self) -> Point<f64, Logical> {
        self.focal
    }

    /// The scale factor of this transform.
    pub fn factor(&self) -> f64 {
        self.factor
    }

    /// Maps a content position to its displayed position.
    pub fn apply(&self, point: Point<f64, Logical>) -> Point<f64, Logical> {
        Point::from((
            self.focal.x + (point.x - self.focal.x) * self.factor,
            self.focal.y + (point.y - self.focal.y) * self.factor,
        ))
    }

    /// Maps a displayed position back to its content position.
    pub fn apply_inverse(&self, point: Point<f64, Logical>) -> Point<f64, Logical> {
        Point::from((
            self.focal.x + (point.x - self.focal.x) / self.factor,
            self.focal.y + (point.y - self.focal.y) / self.factor,
        ))
    }

    /// Maps a content rectangle to its displayed axis-aligned bounding rectangle.
    pub fn apply_rect(&self, rect: Rectangle<f64, Logical>) -> Rectangle<f64, Logical> {
        Rectangle::new(self.apply(rect.loc), rect.size.upscale(self.factor))
    }

    /// Maps a displayed rectangle back to its content axis-aligned bounding rectangle.
    pub fn apply_inverse_rect(&self, rect: Rectangle<f64, Logical>) -> Rectangle<f64, Logical> {
        Rectangle::new(
            self.apply_inverse(rect.loc),
            rect.size.downscale(self.factor),
        )
    }
}

#[cfg(test)]
mod tests {
    use approx::assert_abs_diff_eq;
    use smithay::utils::Size;

    use super::*;

    const EPS: f64 = 1e-9;

    fn assert_point_eq(a: Point<f64, Logical>, b: Point<f64, Logical>) {
        assert_abs_diff_eq!(a.x, b.x, epsilon = EPS);
        assert_abs_diff_eq!(a.y, b.y, epsilon = EPS);
    }

    fn assert_rect_eq(a: Rectangle<f64, Logical>, b: Rectangle<f64, Logical>) {
        assert_point_eq(a.loc, b.loc);
        assert_abs_diff_eq!(a.size.w, b.size.w, epsilon = EPS);
        assert_abs_diff_eq!(a.size.h, b.size.h, epsilon = EPS);
    }

    #[test]
    fn viewport_transform_identity_point() {
        let t = ViewportTransform::identity();
        for (x, y) in [(0., 0.), (10., 20.), (-50., 120.), (12.5, -7.25)] {
            let p = Point::from((x, y));
            assert_eq!(t.apply(p), p);
            assert_eq!(t.apply_inverse(p), p);
        }
    }

    #[test]
    fn viewport_transform_focal_invariant() {
        let focal = Point::from((100., 100.));
        for factor in [1., 1.25, 2., 4.] {
            let t = ViewportTransform::new(focal, factor);
            assert_eq!(t.apply(focal), focal);
        }
    }

    #[test]
    fn viewport_transform_known_point() {
        let t = ViewportTransform::new(Point::from((100., 100.)), 2.);
        assert_eq!(
            t.apply(Point::from((150., 100.))),
            Point::from((200., 100.))
        );
        assert_eq!(
            t.apply(Point::from((100., 150.))),
            Point::from((100., 200.))
        );
        assert_eq!(
            t.apply(Point::from((150., 160.))),
            Point::from((200., 220.))
        );
    }

    #[test]
    fn viewport_transform_point_round_trip() {
        let points = [
            Point::from((0., 0.)),
            Point::from((150., 100.)),
            Point::from((-50., 120.)),
            Point::from((-33.75, 44.5)),
            Point::from((1234.5678, -987.654)),
        ];
        let focal = Point::from((100., 100.));
        for factor in [1.25, 1.5, 2., 4.] {
            let t = ViewportTransform::new(focal, factor);
            for p in points {
                assert_point_eq(t.apply_inverse(t.apply(p)), p);
            }
        }
    }

    #[test]
    fn viewport_transform_inverse_round_trip() {
        let points = [
            Point::from((0., 0.)),
            Point::from((200., 140.)),
            Point::from((-50., 120.)),
            Point::from((-33.75, 44.5)),
        ];
        let focal = Point::from((100., 100.));
        for factor in [1.25, 1.5, 2., 4.] {
            let t = ViewportTransform::new(focal, factor);
            for p in points {
                assert_point_eq(t.apply(t.apply_inverse(p)), p);
            }
        }
    }

    #[test]
    fn viewport_transform_rect() {
        let t = ViewportTransform::new(Point::from((100., 100.)), 2.);
        let rect = Rectangle::new(Point::from((150., 120.)), Size::from((30., 40.)));

        let out = t.apply_rect(rect);
        assert_eq!(out.loc, Point::from((200., 140.)));
        assert_eq!(out.size, Size::from((60., 80.)));

        // The result must equal the bounding rect of the transformed corners.
        let corners = [
            rect.loc,
            rect.loc + Size::from((rect.size.w, 0.)),
            rect.loc + Size::from((0., rect.size.h)),
            rect.loc + rect.size,
        ]
        .map(|p| t.apply(p));

        let min_x = corners.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
        let max_x = corners
            .iter()
            .map(|p| p.x)
            .fold(f64::NEG_INFINITY, f64::max);
        let min_y = corners.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
        let max_y = corners
            .iter()
            .map(|p| p.y)
            .fold(f64::NEG_INFINITY, f64::max);

        let bbox = Rectangle::new(
            Point::from((min_x, min_y)),
            Size::from((max_x - min_x, max_y - min_y)),
        );
        assert_eq!(out, bbox);
    }

    #[test]
    fn viewport_transform_rect_round_trip() {
        let rect = Rectangle::new(Point::from((-33.75, 44.5)), Size::from((12.25, 7.5)));
        let focal = Point::from((100., 100.));
        for factor in [1.25, 1.5, 2., 4.] {
            let t = ViewportTransform::new(focal, factor);
            assert_rect_eq(t.apply_inverse_rect(t.apply_rect(rect)), rect);
        }
    }

    #[test]
    fn viewport_transform_identity_rect() {
        let t = ViewportTransform::identity();
        let rect = Rectangle::new(Point::from((-33.75, 44.5)), Size::from((12.25, 7.5)));
        assert_eq!(t.apply_rect(rect), rect);
        assert_eq!(t.apply_inverse_rect(rect), rect);
    }

    #[test]
    fn viewport_transform_invalid_factor() {
        for factor in [0., -1., f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(
                std::panic::catch_unwind(|| {
                    ViewportTransform::new(Point::from((0., 0.)), factor)
                })
                .is_err()
            );
        }
    }
}
