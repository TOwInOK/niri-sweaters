//! Output-local region frame overlay.
//!
//! Draws a fixed-width border around a validated output-local region. Used for
//! the IPC region frame and as the live preview while a region selection is
//! being dragged. The frame is bound to a single [`Output`]: geometry is
//! validated against the output's integer logical extent at creation and on
//! every update, and each side is clipped to the current output bounds when
//! rendered.

use std::cell::RefCell;

use niri_config::Color;
use niri_ipc::{RegionFrameSpec, RegionGeometry};
use smithay::backend::renderer::element::Kind;
use smithay::output::Output;
use smithay::utils::{Logical, Point, Rectangle, Size};

use crate::render_helpers::solid_color::{SolidColorBuffer, SolidColorRenderElement};
use crate::utils::{inward_border_rects, output_size};

/// Border width in logical pixels.
const BORDER_WIDTH: f64 = 2.;

/// A colored border marking a fixed region of one output.
///
/// The four sides share one color and keep stable [`SolidColorBuffer`]s (and
/// therefore stable element [`Id`](smithay::backend::renderer::element::Id)s)
/// across color and geometry changes, so the damage tracker sees ordinary
/// element updates instead of a brand-new element every frame.
#[derive(Debug)]
pub(crate) struct RegionFrame {
    output: Output,
    spec: RegionFrameSpec,
    /// Buffers for the top, bottom, left, and right sides, resized in place.
    buffers: RefCell<[SolidColorBuffer; 4]>,
}

impl RegionFrame {
    /// Creates a frame for `output` from an IPC spec.
    ///
    /// The spec's output name must match `output`, the color must parse, and
    /// the region must be a non-empty rectangle inside the output's integer
    /// logical bounds (the same extent as `global_space.output_geometry`).
    pub(crate) fn new(output: Output, spec: RegionFrameSpec) -> Result<Self, String> {
        if spec.region.output != output.name() {
            return Err(format!(
                "region output {:?} does not match output {:?}",
                spec.region.output,
                output.name()
            ));
        }
        validate_geometry(spec.region.geometry, &output)?;

        let color = parse_color(&spec.color)?;
        let buffers = RefCell::new([
            SolidColorBuffer::new((0., 0.), color),
            SolidColorBuffer::new((0., 0.), color),
            SolidColorBuffer::new((0., 0.), color),
            SolidColorBuffer::new((0., 0.), color),
        ]);

        let mut frame = Self {
            output,
            spec,
            buffers,
        };
        frame.sync_buffers();
        Ok(frame)
    }

    /// The current IPC spec: output name, geometry, and the color string last
    /// set.
    pub(crate) fn spec(&self) -> RegionFrameSpec {
        self.spec.clone()
    }

    /// The output this frame is bound to.
    pub(crate) fn output(&self) -> &Output {
        &self.output
    }

    /// Replaces the border color, keeping the region.
    ///
    /// The color string is parsed first; on failure the frame is unchanged.
    pub(crate) fn set_color(&mut self, color: String) -> Result<(), String> {
        let parsed = parse_color(&color)?;
        for buffer in self.buffers.get_mut() {
            buffer.set_color(parsed);
        }
        self.spec.color = color;
        Ok(())
    }

    /// Replaces the region, keeping the color.
    ///
    /// The geometry is validated against the output's current integer logical
    /// bounds first; on failure the frame is unchanged.
    pub(crate) fn set_geometry(&mut self, geometry: RegionGeometry) -> Result<(), String> {
        validate_geometry(geometry, &self.output)?;
        self.spec.region.geometry = geometry;
        self.sync_buffers();
        Ok(())
    }

    /// Pushes the four border sides as [`SolidColorRenderElement`]s.
    ///
    /// Does nothing when `output` is not the bound output. Each side is
    /// clipped independently to the output's current integer logical bounds,
    /// so a mode change can only shrink the frame, never push it off-screen.
    /// Geometry is output-local logical; no zoom or overview transform is
    /// applied.
    pub(crate) fn render(&self, output: &Output, push: &mut dyn FnMut(SolidColorRenderElement)) {
        if output != &self.output {
            return;
        }

        let bounds = Rectangle::from_size(output_extent(output)).to_f64();
        let mut buffers = self.buffers.borrow_mut();
        for (rect, buffer) in self.side_rects().into_iter().zip(buffers.iter_mut()) {
            let Some(rect) = rect.intersection(bounds) else {
                continue;
            };
            if rect.is_empty() {
                continue;
            }
            buffer.resize(rect.size);
            push(SolidColorRenderElement::from_buffer(
                buffer,
                rect.loc,
                1.,
                Kind::Unspecified,
            ));
        }
    }

    /// The region as an output-local logical rectangle.
    fn region_rect(&self) -> Rectangle<f64, Logical> {
        let geometry = self.spec.region.geometry;
        Rectangle::new(
            Point::from((f64::from(geometry.x), f64::from(geometry.y))),
            Size::from((f64::from(geometry.width), f64::from(geometry.height))),
        )
    }

    /// Inward border rectangles for the current region: top, bottom, left,
    /// right.
    fn side_rects(&self) -> [Rectangle<f64, Logical>; 4] {
        inward_border_rects(self.region_rect(), BORDER_WIDTH)
    }

    /// Resizes the stable buffers to the current side rectangles.
    fn sync_buffers(&mut self) {
        for (rect, buffer) in self.side_rects().into_iter().zip(self.buffers.get_mut()) {
            buffer.resize(rect.size);
        }
    }
}

/// The output's integer logical extent.
///
/// Matches `global_space.output_geometry`: the logical size ceiled to whole
/// logical pixels, so outward-rounded selections at fractional scales stay
/// inside the accepted bounds.
fn output_extent(output: &Output) -> Size<i32, Logical> {
    output_size(output).to_i32_ceil()
}

/// Checks that `geometry` is a non-empty rectangle inside the output's
/// integer logical bounds.
fn validate_geometry(geometry: RegionGeometry, output: &Output) -> Result<(), String> {
    // Non-zero size and endpoints fitting in i32.
    geometry.validate()?;

    if geometry.x < 0 || geometry.y < 0 {
        return Err(format!(
            "region position ({}, {}) is outside the output",
            geometry.x, geometry.y
        ));
    }

    let extent = output_extent(output);
    let right = i64::from(geometry.x) + i64::from(geometry.width);
    let bottom = i64::from(geometry.y) + i64::from(geometry.height);
    if right > i64::from(extent.w) || bottom > i64::from(extent.h) {
        return Err(format!(
            "region {}x{} at ({}, {}) exceeds the {}x{} output bounds",
            geometry.width, geometry.height, geometry.x, geometry.y, extent.w, extent.h
        ));
    }
    Ok(())
}

fn parse_color(color: &str) -> Result<Color, String> {
    color
        .parse::<Color>()
        .map_err(|err| format!("invalid color {color:?}: {err}"))
}

#[cfg(test)]
mod tests {
    use niri_ipc::OutputRegion;
    use smithay::backend::renderer::Color32F;
    use smithay::output::{Mode, PhysicalProperties, Scale, Subpixel};

    use super::*;

    fn test_output(name: &str, physical: (i32, i32), scale: f64) -> Output {
        let output = Output::new(
            name.to_string(),
            PhysicalProperties {
                size: Size::from((0, 0)),
                subpixel: Subpixel::Unknown,
                make: String::new(),
                model: String::new(),
                serial_number: String::new(),
            },
        );
        output.change_current_state(
            Some(Mode {
                size: Size::from(physical),
                refresh: 60000,
            }),
            None,
            Some(Scale::Fractional(scale)),
            None,
        );
        output
    }

    fn spec(output: &str, geometry: RegionGeometry) -> RegionFrameSpec {
        RegionFrameSpec {
            region: OutputRegion {
                output: output.to_string(),
                geometry,
            },
            color: "red".to_string(),
        }
    }

    fn render(frame: &RegionFrame, output: &Output) -> Vec<SolidColorRenderElement> {
        let mut elements = Vec::new();
        frame.render(output, &mut |element| elements.push(element));
        elements
    }

    #[test]
    fn rejects_invalid_geometry() {
        let output = test_output("out", (1920, 1080), 1.);
        let ok = RegionGeometry {
            x: 10,
            y: 20,
            width: 100,
            height: 50,
        };
        assert!(RegionFrame::new(output.clone(), spec("out", ok)).is_ok());

        for geometry in [
            RegionGeometry { x: -1, ..ok },
            RegionGeometry { y: -1, ..ok },
            RegionGeometry { width: 0, ..ok },
            RegionGeometry { height: 0, ..ok },
            // Right/bottom edges past the bounds.
            RegionGeometry { x: 1920 - 99, ..ok },
            RegionGeometry { y: 1080 - 49, ..ok },
            RegionGeometry {
                x: 0,
                y: 0,
                width: 1921,
                height: 1080,
            },
        ] {
            assert!(
                RegionFrame::new(output.clone(), spec("out", geometry)).is_err(),
                "{geometry:?}"
            );
        }

        // Edge-touching regions are accepted.
        let edge = RegionGeometry {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        assert!(RegionFrame::new(output, spec("out", edge)).is_ok());
    }

    #[test]
    fn accepts_outward_rounded_extent_at_fractional_scale() {
        // 1920/1.75 = 1097.14… and 1080/1.75 = 617.14…; the integer logical
        // extent ceils to 1098x618, matching global_space.output_geometry.
        let output = test_output("out", (1920, 1080), 1.75);
        let full = RegionGeometry {
            x: 0,
            y: 0,
            width: 1098,
            height: 618,
        };
        assert!(RegionFrame::new(output.clone(), spec("out", full)).is_ok());

        // One pixel past the extent is still rejected.
        let past = RegionGeometry {
            width: 1099,
            ..full
        };
        assert!(RegionFrame::new(output, spec("out", past)).is_err());
    }

    #[test]
    fn rejects_output_name_mismatch() {
        let output = test_output("out", (1920, 1080), 1.);
        let geometry = RegionGeometry {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        };
        assert!(RegionFrame::new(output, spec("other", geometry)).is_err());
    }

    #[test]
    fn render_clips_to_current_output_size() {
        let output = test_output("out", (1920, 1080), 1.);
        let geometry = RegionGeometry {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let frame = RegionFrame::new(output.clone(), spec("out", geometry)).unwrap();

        // Shrink the output horizontally; the right side ends up fully
        // off-screen and the horizontal bars are clipped.
        output.change_current_state(
            Some(Mode {
                size: Size::from((1000, 1080)),
                refresh: 60000,
            }),
            None,
            None,
            None,
        );

        let elements = render(&frame, &output);
        let bounds = Rectangle::from_size(Size::from((1000., 1080.)));
        assert_eq!(elements.len(), 3);
        for element in &elements {
            assert!(bounds.contains_rect(element.geo()));
        }
    }

    #[test]
    fn render_ignores_other_outputs() {
        let output = test_output("out", (1920, 1080), 1.);
        let other = test_output("other", (1920, 1080), 1.);
        let geometry = RegionGeometry {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        };
        let frame = RegionFrame::new(output, spec("out", geometry)).unwrap();
        assert!(render(&frame, &other).is_empty());
    }

    #[test]
    fn set_color_is_atomic() {
        let output = test_output("out", (1920, 1080), 1.);
        let geometry = RegionGeometry {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        };
        let mut frame = RegionFrame::new(output.clone(), spec("out", geometry)).unwrap();

        // An unparsable color leaves the frame unchanged.
        assert!(frame.set_color("not a color".to_string()).is_err());
        assert_eq!(frame.spec().color, "red");

        let before: Vec<_> = render(&frame, &output)
            .iter()
            .map(|element| element.geo())
            .collect();
        frame.set_color("#00ff00".to_string()).unwrap();
        assert_eq!(frame.spec().color, "#00ff00");

        let after = render(&frame, &output);
        let expected = Color32F::from([0., 1., 0., 1.]);
        for element in &after {
            assert_eq!(element.color(), expected);
        }
        // Geometry is untouched.
        let geos: Vec<_> = after.iter().map(|element| element.geo()).collect();
        assert_eq!(geos, before);
    }

    #[test]
    fn set_geometry_updates_rendered_border() {
        let output = test_output("out", (1920, 1080), 1.);
        let geometry = RegionGeometry {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        };
        let mut frame = RegionFrame::new(output.clone(), spec("out", geometry)).unwrap();

        frame
            .set_geometry(RegionGeometry {
                x: 50,
                y: 50,
                width: 200,
                height: 200,
            })
            .unwrap();
        let expected = [
            Rectangle::new((50., 50.).into(), (200., 2.).into()),
            Rectangle::new((50., 248.).into(), (200., 2.).into()),
            Rectangle::new((50., 52.).into(), (2., 196.).into()),
            Rectangle::new((248., 52.).into(), (2., 196.).into()),
        ];
        let geos: Vec<_> = render(&frame, &output)
            .iter()
            .map(|element| element.geo())
            .collect();
        assert_eq!(geos, expected);

        // Invalid geometry leaves the rendered frame unchanged.
        assert!(frame
            .set_geometry(RegionGeometry {
                x: -5,
                y: 0,
                width: 10,
                height: 10,
            })
            .is_err());
        let geos: Vec<_> = render(&frame, &output)
            .iter()
            .map(|element| element.geo())
            .collect();
        assert_eq!(geos, expected);
    }
}
