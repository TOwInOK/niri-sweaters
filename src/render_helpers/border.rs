use std::collections::HashMap;
use std::rc::Rc;

use glam::{Mat3, Vec2};
use niri_config::{
    Color, CornerRadius, GradientColorSpace, GradientInterpolation, HueInterpolation, KnitBorder,
    KnitPattern,
};
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{GlesError, GlesFrame, GlesRenderer, Uniform};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::gpu_span_location;
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform};

use super::renderer::NiriRenderer;
use super::shader_element::ShaderRenderElement;
use super::shaders::{mat3_uniform, ProgramType, Shaders};
use crate::backend::tty::{TtyFrame, TtyRenderer, TtyRendererError};
use crate::render_helpers::renderer::AsGlesFrame as _;

/// Renders a wide variety of borders and border parts.
///
/// This includes:
/// * sub- or super-rect of an angled linear gradient like CSS linear-gradient(angle, a, b).
/// * corner rounding.
/// * as a background rectangle and as parts of a border line.
#[derive(Debug, Clone)]
pub struct BorderRenderElement {
    inner: ShaderRenderElement,
    params: Parameters,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Parameters {
    size: Size<f64, Logical>,
    gradient_area: Rectangle<f64, Logical>,
    gradient_format: GradientInterpolation,
    color_from: Color,
    color_to: Color,
    angle: f32,
    geometry: Rectangle<f64, Logical>,
    border_width: f32,
    corner_radius: CornerRadius,
    // Should only be used for visual improvements, i.e. corner radius anti-aliasing.
    scale: f32,
    alpha: f32,
    knit: Option<KnitBorder>,
}

impl BorderRenderElement {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        size: Size<f64, Logical>,
        gradient_area: Rectangle<f64, Logical>,
        gradient_format: GradientInterpolation,
        color_from: Color,
        color_to: Color,
        angle: f32,
        geometry: Rectangle<f64, Logical>,
        border_width: f32,
        corner_radius: CornerRadius,
        scale: f32,
        alpha: f32,
    ) -> Self {
        let inner = ShaderRenderElement::empty(ProgramType::Border, Kind::Unspecified);
        let mut rv = Self {
            inner,
            params: Parameters {
                size,
                gradient_area,
                gradient_format,
                color_from,
                color_to,
                angle,
                geometry,
                border_width,
                corner_radius,
                scale,
                alpha,
                knit: None,
            },
        };
        rv.update_inner();
        rv
    }

    pub fn empty() -> Self {
        let inner = ShaderRenderElement::empty(ProgramType::Border, Kind::Unspecified);
        Self {
            inner,
            params: Parameters {
                size: Default::default(),
                gradient_area: Default::default(),
                gradient_format: GradientInterpolation::default(),
                color_from: Default::default(),
                color_to: Default::default(),
                angle: 0.,
                geometry: Default::default(),
                border_width: 0.,
                corner_radius: Default::default(),
                scale: 1.,
                alpha: 1.,
                knit: None,
            },
        }
    }

    pub fn damage_all(&mut self) {
        self.inner.damage_all();
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        size: Size<f64, Logical>,
        gradient_area: Rectangle<f64, Logical>,
        gradient_format: GradientInterpolation,
        color_from: Color,
        color_to: Color,
        angle: f32,
        geometry: Rectangle<f64, Logical>,
        border_width: f32,
        corner_radius: CornerRadius,
        scale: f32,
        alpha: f32,
    ) {
        self.update_with_knit(
            size,
            gradient_area,
            gradient_format,
            color_from,
            color_to,
            angle,
            geometry,
            border_width,
            corner_radius,
            scale,
            alpha,
            None,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_with_knit(
        &mut self,
        size: Size<f64, Logical>,
        gradient_area: Rectangle<f64, Logical>,
        gradient_format: GradientInterpolation,
        color_from: Color,
        color_to: Color,
        angle: f32,
        geometry: Rectangle<f64, Logical>,
        border_width: f32,
        corner_radius: CornerRadius,
        scale: f32,
        alpha: f32,
        knit: Option<KnitBorder>,
    ) {
        let params = Parameters {
            size,
            gradient_area,
            gradient_format,
            color_from,
            color_to,
            angle,
            geometry,
            border_width,
            corner_radius,
            scale,
            alpha,
            knit,
        };
        if self.params == params {
            return;
        }

        self.params = params;
        self.update_inner();
    }

    fn update_inner(&mut self) {
        let Parameters {
            size,
            gradient_area,
            gradient_format,
            color_from,
            color_to,
            angle,
            geometry,
            border_width,
            corner_radius,
            scale,
            alpha,
            knit,
        } = self.params;

        let grad_offset = geometry.loc - gradient_area.loc;
        let grad_offset = Vec2::new(grad_offset.x as f32, grad_offset.y as f32);

        let grad_dir = Vec2::from_angle(angle);

        let (w, h) = (gradient_area.size.w as f32, gradient_area.size.h as f32);

        let mut grad_area_diag = Vec2::new(w, h);
        if (grad_dir.x < 0. && 0. <= grad_dir.y) || (0. <= grad_dir.x && grad_dir.y < 0.) {
            grad_area_diag.x = -w;
        }

        let mut grad_vec = grad_area_diag.project_onto(grad_dir);
        if grad_dir.y < 0. {
            grad_vec = -grad_vec;
        }

        let area_size = Vec2::new(size.w as f32, size.h as f32);

        let geo_loc = Vec2::new(geometry.loc.x as f32, geometry.loc.y as f32);
        let geo_size = Vec2::new(geometry.size.w as f32, geometry.size.h as f32);

        let input_to_geo =
            Mat3::from_scale(area_size) * Mat3::from_translation(-geo_loc / area_size);

        let colorspace = match gradient_format.color_space {
            GradientColorSpace::Srgb => 0.,
            GradientColorSpace::SrgbLinear => 1.,
            GradientColorSpace::Oklab => 2.,
            GradientColorSpace::Oklch => 3.,
        };

        let hue_interpolation = match gradient_format.hue_interpolation {
            HueInterpolation::Shorter => 0.,
            HueInterpolation::Longer => 1.,
            HueInterpolation::Increasing => 2.,
            HueInterpolation::Decreasing => 3.,
        };

        // Convert the gradient endpoints into the interpolation space once per
        // element instead of per pixel in the shader.
        let convert = |color: Color| -> [f32; 4] {
            let [r, g, b, a] = color.to_array_unpremul();
            let rgb = match gradient_format.color_space {
                GradientColorSpace::Srgb => [r, g, b],
                GradientColorSpace::SrgbLinear => srgb_to_linear([r, g, b]),
                GradientColorSpace::Oklab => linear_to_oklab(srgb_to_linear([r, g, b])),
                GradientColorSpace::Oklch => {
                    oklab_to_oklch(linear_to_oklab(srgb_to_linear([r, g, b])))
                }
            };
            [rgb[0], rgb[1], rgb[2], a]
        };
        let color_from = convert(color_from);
        let color_to = convert(color_to);

        let grad_inv_dot = 1. / grad_vec.length_squared().max(1e-6);

        let (
            knit_enabled,
            knit_pattern,
            knit_accent_color,
            knit_stitch_size,
            knit_relief,
            knit_fuzz,
            knit_motif_bends,
        ) = if let Some(knit) = knit {
            let pattern = match knit.pattern {
                KnitPattern::Stockinette => 0.,
                KnitPattern::Rib => 1.,
                KnitPattern::Checker => 2.,
                KnitPattern::Zigzag => 3.,
                KnitPattern::Diamond => 4.,
                KnitPattern::Dots => 5.,
            };
            // Reference bend counts align motif colours across rows. The shader
            // only needs this vec4, so it is derived here once per element
            // instead of per pixel. Mirrors knit_course() in border.frag.
            let stitch_width = (knit.stitch_size as f32).max(1.0);
            let inv_stitch_width = 1.0 / stitch_width;
            let depth = ((border_width - 0.5).max(0.0) * 0.5)
                .min(geo_size.x.min(geo_size.y) * 0.49);
            let radii = <[f32; 4]>::from(corner_radius);
            let mut motif_bends = [0.0f32; 4];
            for (i, bend) in motif_bends.iter_mut().enumerate() {
                let radius = (radii[(i + 1) % 4] - depth).max(0.0);
                *bend = (radius * 1.57079633 * inv_stitch_width + 0.5)
                    .floor()
                    .max(1.0)
                    * if radius >= 0.001 { 1.0 } else { 0.0 };
            }
            (
                1.,
                pattern,
                knit.accent_color.to_array_unpremul(),
                knit.stitch_size as f32,
                knit.relief as f32,
                knit.fuzz as f32,
                motif_bends,
            )
        } else {
            (0., 0., Color::default().to_array_unpremul(), 1., 0., 0., [0.; 4])
        };
        self.inner.update(
            size,
            None,
            scale,
            alpha,
            Rc::new([
                Uniform::new("colorspace", colorspace),
                Uniform::new("hue_interpolation", hue_interpolation),
                Uniform::new("color_from", color_from),
                Uniform::new("color_to", color_to),
                Uniform::new("grad_offset", grad_offset.to_array()),
                Uniform::new("grad_width", w),
                Uniform::new("grad_vec", grad_vec.to_array()),
                Uniform::new("grad_inv_dot", grad_inv_dot),
                mat3_uniform("input_to_geo", input_to_geo),
                Uniform::new("geo_size", geo_size.to_array()),
                Uniform::new("outer_radius", <[f32; 4]>::from(corner_radius)),
                Uniform::new("border_width", border_width),
                Uniform::new("knit_enabled", knit_enabled),
                Uniform::new("knit_pattern", knit_pattern),
                Uniform::new("knit_accent_color", knit_accent_color),
                Uniform::new("knit_stitch_size", knit_stitch_size),
                Uniform::new("knit_relief", knit_relief),
                Uniform::new("knit_fuzz", knit_fuzz),
                Uniform::new("knit_motif_bends", knit_motif_bends),
            ]),
            HashMap::new(),
        );
    }

    pub fn with_location(mut self, location: Point<f64, Logical>) -> Self {
        self.inner = self.inner.with_location(location);
        self
    }

    pub fn has_shader(renderer: &mut impl NiriRenderer) -> bool {
        Shaders::get(renderer)
            .program(ProgramType::Border)
            .is_some()
    }
    #[cfg(test)]
    pub(crate) fn knit_for_tests(&self) -> Option<KnitBorder> {
        self.params.knit
    }
}

impl Default for BorderRenderElement {
    fn default() -> Self {
        Self::empty()
    }
}

impl Element for BorderRenderElement {
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        self.inner.opaque_regions(scale)
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl RenderElement<GlesRenderer> for BorderRenderElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let _span = tracy_client::span!("BorderRenderElement::draw");
        frame.with_gpu_span(gpu_span_location!("BorderRenderElement::draw"), |frame| {
            RenderElement::<GlesRenderer>::draw(
                &self.inner,
                frame,
                src,
                dst,
                damage,
                opaque_regions,
                cache,
            )
        })
    }

    fn underlying_storage(&self, renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        self.inner.underlying_storage(renderer)
    }
}

impl<'render> RenderElement<TtyRenderer<'render>> for BorderRenderElement {
    fn draw(
        &self,
        frame: &mut TtyFrame<'_, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), TtyRendererError<'render>> {
        let frame = frame.as_gles_frame();
        RenderElement::<GlesRenderer>::draw(self, frame, src, dst, damage, opaque_regions, cache)?;
        Ok(())
    }

    fn underlying_storage(
        &self,
        renderer: &mut TtyRenderer<'render>,
    ) -> Option<UnderlyingStorage<'_>> {
        self.inner.underlying_storage(renderer)
    }
}

// Color space conversions matching border.frag. They run once per element on
// the CPU so the shader only needs the inverse transforms per pixel.

fn srgb_to_linear([r, g, b]: [f32; 3]) -> [f32; 3] {
    [r.powf(2.2), g.powf(2.2), b.powf(2.2)]
}

fn linear_to_oklab([r, g, b]: [f32; 3]) -> [f32; 3] {
    // Row-vector times column-major matrix, same as `color * mat` in GLSL.
    let l = 0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b;
    let m = 0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b;
    let s = 0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b;
    let l = l.powf(1. / 3.);
    let m = m.powf(1. / 3.);
    let s = s.powf(1. / 3.);
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

fn oklab_to_oklch([l, a, b]: [f32; 3]) -> [f32; 3] {
    let c = (a * a + b * b).sqrt();
    let mut h = b.atan2(a).to_degrees();
    if h <= 0. {
        h += 360.;
    }
    [l, c, h]
}
