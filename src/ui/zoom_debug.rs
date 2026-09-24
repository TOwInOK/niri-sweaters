//! Debug visualization of the desktop zoom state.
//!
//! Draws screen-space [`SolidColorRenderElement`]s on the physical output:
//! the deadzone outline and the focal point marker. This module only turns
//! already-computed zoom state into geometry; it never reads or mutates the
//! zoom state itself.

use niri_config::Color;
use smithay::backend::renderer::Color32F;
use smithay::backend::renderer::element::{Id, Kind};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::utils::{Logical, Point, Rectangle, Size};

use crate::niri::OutputRenderElements;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::solid_color::SolidColorRenderElement;
use crate::utils::{center_f64, inward_border_rects};

/// Red outline of the deadzone rectangle.
pub(crate) const DEADZONE_COLOR: Color =
    Color::from_array_unpremul([1., 0x3B as f32 / 255., 0x30 as f32 / 255., 1.]);
/// Amber focal point marker while the zoom level is above 1.
pub(crate) const FOCAL_ACTIVE_COLOR: Color =
    Color::from_array_unpremul([1., 0xB0 as f32 / 255., 0., 1.]);
/// Amber focal point marker at level 1: the stored focal point does not
/// affect the identity transform, so it is drawn dimmed.
pub(crate) const FOCAL_INACTIVE_COLOR: Color =
    Color::from_array_unpremul([1., 0xB0 as f32 / 255., 0., 0.45]);
/// Near-black halo drawn under the main strokes for contrast.
pub(crate) const HALO_COLOR: Color = Color::from_array_unpremul([0., 0., 0., 0.65]);

/// Main stroke width in logical pixels.
const STROKE_WIDTH: f64 = 2.;
/// Halo stroke width in logical pixels.
const HALO_WIDTH: f64 = 4.;
/// Total extent of the focal crosshair in logical pixels.
const FOCAL_EXTENT: f64 = 14.;
/// Side of the focal center square in logical pixels.
const FOCAL_CENTER: f64 = 4.;
/// Total extent of the crosshair marking a zero-area deadzone.
const DEADZONE_POINT_EXTENT: f64 = 14.;

/// Horizontal and vertical bars of a crosshair centered on `center`.
///
/// `extent` is the total length of each bar; `width` is the bar thickness.
/// Both bars are centered on `center`, so the crosshair is symmetric.
pub(crate) fn crosshair_rects(
    center: Point<f64, Logical>,
    extent: f64,
    width: f64,
) -> [Rectangle<f64, Logical>; 2] {
    let half = extent / 2.;
    let half_w = width / 2.;

    let horizontal = Rectangle::new(
        Point::from((center.x - half, center.y - half_w)),
        Size::from((extent, width)),
    );
    let vertical = Rectangle::new(
        Point::from((center.x - half_w, center.y - half)),
        Size::from((width, extent)),
    );

    [horizontal, vertical]
}

fn push_rect<R: NiriRenderer>(
    rect: Rectangle<f64, Logical>,
    color: Color,
    push: &mut dyn FnMut(OutputRenderElements<R>),
) {
    if rect.is_empty() {
        return;
    }

    push(
        SolidColorRenderElement::new(
            Id::new(),
            rect,
            CommitCounter::default(),
            Color32F::from(color.to_array_premul()),
            Kind::Unspecified,
        )
        .into(),
    );
}

fn push_rects<R: NiriRenderer>(
    rects: impl IntoIterator<Item = Rectangle<f64, Logical>>,
    color: Color,
    push: &mut dyn FnMut(OutputRenderElements<R>),
) {
    for rect in rects {
        push_rect(rect, color, push);
    }
}

/// Draws the deadzone outline: a cyan stroke over a dark halo for contrast.
///
/// `deadzone` is the rect from
/// [`OutputZoomState::deadzone_rect`](crate::layout::zoom::OutputZoomState::deadzone_rect)
/// in output-local logical coordinates. A zero-area deadzone is drawn as a
/// small crosshair on its center instead of a degenerate border.
pub(crate) fn render_deadzone<R: NiriRenderer>(
    deadzone: Rectangle<f64, Logical>,
    push: &mut dyn FnMut(OutputRenderElements<R>),
) {
    if deadzone.is_empty() {
        let center = center_f64(deadzone);
        // Same order as below: the main crosshair on top of its halo.
        push_rects(
            crosshair_rects(center, DEADZONE_POINT_EXTENT, STROKE_WIDTH),
            DEADZONE_COLOR,
            push,
        );
        push_rects(
            crosshair_rects(center, DEADZONE_POINT_EXTENT, HALO_WIDTH),
            HALO_COLOR,
            push,
        );
        return;
    }

    // Elements are drawn in reverse push order: the last pushed element is
    // at the bottom. The halo goes last so that it sits under the main stroke.
    push_rects(
        inward_border_rects(deadzone, STROKE_WIDTH),
        DEADZONE_COLOR,
        push,
    );
    push_rects(inward_border_rects(deadzone, HALO_WIDTH), HALO_COLOR, push);
}

/// Draws the focal point marker: a crosshair with a center square.
///
/// `focal` is the fixed point of the current viewport transform in
/// output-local logical coordinates, drawn as-is without re-applying the
/// transform. `active` selects the full-opacity style used while the zoom
/// level is above 1; at level 1 the stored focal point is drawn dimmed.
pub(crate) fn render_focal<R: NiriRenderer>(
    focal: Point<f64, Logical>,
    active: bool,
    push: &mut dyn FnMut(OutputRenderElements<R>),
) {
    let color = if active {
        FOCAL_ACTIVE_COLOR
    } else {
        FOCAL_INACTIVE_COLOR
    };

    // Elements are drawn in reverse push order: the last pushed element is
    // at the bottom. The center square is on top, then the crosshair, then
    // the halo underneath both.
    let half = FOCAL_CENTER / 2.;
    let center = Rectangle::new(
        Point::from((focal.x - half, focal.y - half)),
        Size::from((FOCAL_CENTER, FOCAL_CENTER)),
    );
    push_rect(center, color, push);

    push_rects(
        crosshair_rects(focal, FOCAL_EXTENT, STROKE_WIDTH),
        color,
        push,
    );
    push_rects(
        crosshair_rects(focal, FOCAL_EXTENT, HALO_WIDTH),
        HALO_COLOR,
        push,
    );
}

#[cfg(test)]
mod tests {
    use approx::assert_abs_diff_eq;

    use super::*;

    fn assert_rect_eq(actual: Rectangle<f64, Logical>, expected: Rectangle<f64, Logical>) {
        assert_abs_diff_eq!(actual.loc.x, expected.loc.x);
        assert_abs_diff_eq!(actual.loc.y, expected.loc.y);
        assert_abs_diff_eq!(actual.size.w, expected.size.w);
        assert_abs_diff_eq!(actual.size.h, expected.size.h);
    }

    #[test]
    fn crosshair_is_symmetric() {
        let center = Point::from((960., 360.));
        let [horizontal, vertical] = crosshair_rects(center, 14., 2.);

        assert_rect_eq(
            horizontal,
            Rectangle::new(Point::from((953., 359.)), (14., 2.).into()),
        );
        assert_rect_eq(
            vertical,
            Rectangle::new(Point::from((959., 353.)), (2., 14.).into()),
        );

        assert_eq!(center_f64(horizontal), center);
        assert_eq!(center_f64(vertical), center);
    }

    #[test]
    fn zero_area_deadzone_center() {
        // deadzone-size 0 degenerates to the output center point; the
        // crosshair geometry is centered on it.
        let deadzone = Rectangle::new(Point::from((960., 360.)), Size::from((0., 0.)));
        assert!(deadzone.is_empty());
        assert_eq!(center_f64(deadzone), Point::from((960., 360.)));
    }
}
