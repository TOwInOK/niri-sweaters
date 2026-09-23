use std::cell::Cell;

use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{GlesError, GlesFrame, GlesRenderer};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::backend::renderer::{FrameContext as _, Renderer as _, TextureFilter};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Physical, Point, Rectangle, Scale, Transform};

use super::renderer::AsGlesFrame as _;
use crate::backend::tty::{TtyFrame, TtyRenderer, TtyRendererError};

/// Returns the cell tracking the frame's renderer upscale filter.
///
/// Stored in the renderer's [`EGLContext`](smithay::backend::egl::EGLContext) user data so that it
/// is scoped to the renderer and does not leak globally. `GlesRenderer` does not expose a getter
/// for its upscale filter, so we track it ourselves; the renderer starts out with
/// [`TextureFilter::Linear`] and only this module changes it.
fn scoped_filter<'a>(frame: &'a GlesFrame<'_, '_>) -> &'a Cell<TextureFilter> {
    let data = frame.egl_context().user_data();
    data.get_or_insert(|| Cell::new(TextureFilter::Linear))
}

/// Returns the upscale filter currently in effect for the frame's renderer.
///
/// This is [`TextureFilter::Linear`] unless a [`SamplingRenderElement`] draw is in progress.
pub fn current_upscale_filter(frame: &GlesFrame<'_, '_>) -> TextureFilter {
    let data = frame.egl_context().user_data();
    data.get::<Cell<TextureFilter>>()
        .map(Cell::get)
        .unwrap_or(TextureFilter::Linear)
}

/// Sets the renderer's upscale filter and returns the previous one for [`restore_upscale_filter`].
///
/// Does nothing when the requested filter is already active, avoiding the
/// `FrameContext::renderer()` guard (and its render-target save/restore) for the common no-op case.
fn set_upscale_filter(
    frame: &mut GlesFrame<'_, '_>,
    filter: TextureFilter,
) -> Result<TextureFilter, GlesError> {
    let prev = scoped_filter(frame).get();
    if prev == filter {
        return Ok(prev);
    }

    frame.renderer().as_mut().upscale_filter(filter)?;
    scoped_filter(frame).set(filter);

    Ok(prev)
}

/// Restores the upscale filter returned by the matching [`set_upscale_filter`] call.
fn restore_upscale_filter(
    frame: &mut GlesFrame<'_, '_>,
    prev: TextureFilter,
) -> Result<(), GlesError> {
    if scoped_filter(frame).get() == prev {
        return Ok(());
    }

    frame.renderer().as_mut().upscale_filter(prev)?;
    scoped_filter(frame).set(prev);

    Ok(())
}

/// A render element that draws its inner element with a specific texture upscale filter.
///
/// The filter is scoped to the `draw()` call only: it is set right before the inner element draws
/// and restored afterwards, including on error and for nested elements. `capture_framebuffer()` is
/// unaffected.
///
/// When `nearest` is `true`, the element is drawn with [`TextureFilter::Nearest`] and reports no
/// [`UnderlyingStorage`], preventing direct scanout which would bypass the filter. When `nearest`
/// is `false`, the element is drawn with [`TextureFilter::Linear`], which is a no-op in the common
/// case and costs no renderer context switch.
#[derive(Debug)]
pub struct SamplingRenderElement<E> {
    element: E,
    nearest: bool,
}

impl<E> SamplingRenderElement<E> {
    pub fn new(element: E, nearest: bool) -> Self {
        Self { element, nearest }
    }
}

impl<E: Element> Element for SamplingRenderElement<E> {
    fn id(&self) -> &Id {
        self.element.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.element.current_commit()
    }

    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.element.location(scale)
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.element.src()
    }

    fn transform(&self) -> Transform {
        self.element.transform()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.element.geometry(scale)
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.element.damage_since(scale, commit)
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        self.element.opaque_regions(scale)
    }

    fn alpha(&self) -> f32 {
        self.element.alpha()
    }

    fn kind(&self) -> Kind {
        self.element.kind()
    }

    fn is_framebuffer_effect(&self) -> bool {
        self.element.is_framebuffer_effect()
    }
}

impl<E: RenderElement<GlesRenderer>> RenderElement<GlesRenderer> for SamplingRenderElement<E> {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let filter = if self.nearest {
            TextureFilter::Nearest
        } else {
            TextureFilter::Linear
        };

        let prev = set_upscale_filter(frame, filter)?;
        let res = self
            .element
            .draw(frame, src, dst, damage, opaque_regions, cache);
        let restore = restore_upscale_filter(frame, prev);

        res.and(restore)
    }

    fn underlying_storage(&self, renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        if self.nearest {
            // Direct scanout would bypass the GL filter.
            None
        } else {
            self.element.underlying_storage(renderer)
        }
    }

    fn capture_framebuffer(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), GlesError> {
        // The filter is scoped to draw() only.
        self.element.capture_framebuffer(frame, src, dst, cache)
    }
}

impl<'render, E: RenderElement<TtyRenderer<'render>>> RenderElement<TtyRenderer<'render>>
    for SamplingRenderElement<E>
{
    fn draw(
        &self,
        frame: &mut TtyFrame<'render, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), TtyRendererError<'render>> {
        let filter = if self.nearest {
            TextureFilter::Nearest
        } else {
            TextureFilter::Linear
        };

        let prev = set_upscale_filter(frame.as_gles_frame(), filter)?;
        let res = self
            .element
            .draw(frame, src, dst, damage, opaque_regions, cache);
        let restore =
            restore_upscale_filter(frame.as_gles_frame(), prev).map_err(TtyRendererError::from);

        res.and(restore)
    }

    fn underlying_storage(
        &self,
        renderer: &mut TtyRenderer<'render>,
    ) -> Option<UnderlyingStorage<'_>> {
        if self.nearest {
            // Direct scanout would bypass the GL filter.
            None
        } else {
            self.element.underlying_storage(renderer)
        }
    }

    fn capture_framebuffer(
        &self,
        frame: &mut TtyFrame<'render, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), TtyRendererError<'render>> {
        // The filter is scoped to draw() only.
        self.element.capture_framebuffer(frame, src, dst, cache)
    }
}

#[cfg(test)]
mod tests {
    use smithay::backend::allocator::Fourcc;
    use smithay::backend::egl::native::EGLSurfacelessDisplay;
    use smithay::backend::egl::{EGLContext, EGLDisplay};
    use smithay::backend::renderer::element::memory::{
        MemoryRenderBuffer, MemoryRenderBufferRenderElement,
    };
    use smithay::backend::renderer::gles::GlesTexture;
    use smithay::backend::renderer::{Bind, ExportMem, Frame as _, Offscreen};
    use smithay::utils::Size;

    use super::*;

    /// An element whose draw always fails, to exercise filter restoration on error.
    struct FailElement {
        id: Id,
    }

    impl Element for FailElement {
        fn id(&self) -> &Id {
            &self.id
        }

        fn current_commit(&self) -> CommitCounter {
            CommitCounter::default()
        }

        fn src(&self) -> Rectangle<f64, Buffer> {
            Rectangle::from_size(Size::from((1., 1.)))
        }

        fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
            Rectangle::from_size(Size::from((64, 64)))
        }
    }

    impl RenderElement<GlesRenderer> for FailElement {
        fn draw(
            &self,
            _frame: &mut GlesFrame<'_, '_>,
            _src: Rectangle<f64, Buffer>,
            _dst: Rectangle<i32, Physical>,
            _damage: &[Rectangle<i32, Physical>],
            _opaque_regions: &[Rectangle<i32, Physical>],
            _cache: Option<&UserDataMap>,
        ) -> Result<(), GlesError> {
            Err(GlesError::UnknownUniform("fail".to_string()))
        }
    }

    fn make_renderer() -> GlesRenderer {
        crate::tests::fixture::prefer_mesa_egl();
        unsafe {
            let display = EGLDisplay::new(EGLSurfacelessDisplay).unwrap();
            let context = EGLContext::new(&display).unwrap();
            GlesRenderer::new(context).unwrap()
        }
    }

    /// A 2x1 red|blue texture element upscaled to 64x64.
    fn texture_elem(renderer: &mut GlesRenderer) -> MemoryRenderBufferRenderElement<GlesRenderer> {
        let buf = MemoryRenderBuffer::from_slice(
            &[255, 0, 0, 255, 0, 0, 255, 255],
            Fourcc::Abgr8888,
            (2, 1),
            1,
            Transform::Normal,
            None,
        );
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            (0., 0.),
            &buf,
            None,
            Some(Rectangle::from_size(Size::from((2., 1.)))),
            Some(Size::from((64, 64))),
            Kind::Unspecified,
        )
        .unwrap()
    }

    /// Renders `elem` into a 64x64 offscreen target and returns the Abgr8888 pixels.
    fn render_pixels(
        renderer: &mut GlesRenderer,
        elem: &impl RenderElement<GlesRenderer>,
    ) -> Vec<u8> {
        let buf_size = Size::<i32, Buffer>::from((64, 64));
        let out_size = Size::<i32, Physical>::from((64, 64));
        let mut texture: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, buf_size).unwrap();
        let mut target = renderer.bind(&mut texture).unwrap();

        let mut frame = renderer
            .render(&mut target, out_size, Transform::Normal)
            .unwrap();
        let dst = elem.geometry(Scale::from(1.));
        elem.draw(&mut frame, elem.src(), dst, &[dst], &[], None)
            .unwrap();
        let _ = frame.finish().unwrap();

        let mapping = renderer
            .copy_framebuffer(&target, Rectangle::from_size(buf_size), Fourcc::Abgr8888)
            .unwrap();
        renderer.map_texture(&mapping).unwrap().to_vec()
    }

    fn pixel(pixels: &[u8], x: usize, y: usize) -> [u8; 4] {
        let i = (y * 64 + x) * 4;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    }

    #[test]
    fn nearest_and_linear() {
        let mut renderer = make_renderer();

        let nearest_elem = SamplingRenderElement::new(texture_elem(&mut renderer), true);
        // Direct scanout would bypass the GL filter.
        assert!(nearest_elem.underlying_storage(&mut renderer).is_none());

        let linear_elem = SamplingRenderElement::new(texture_elem(&mut renderer), false);
        assert!(matches!(
            linear_elem.underlying_storage(&mut renderer),
            Some(UnderlyingStorage::Memory(_))
        ));

        // Nearest: hard edge at x=32.
        let pixels = render_pixels(&mut renderer, &nearest_elem);
        assert_eq!(pixel(&pixels, 31, 32)[..3], [255, 0, 0]);
        assert_eq!(pixel(&pixels, 32, 32)[..3], [0, 0, 255]);

        // Linear: blended edge.
        let pixels = render_pixels(&mut renderer, &linear_elem);
        let edge = pixel(&pixels, 31, 32);
        assert!(
            edge[0] > 0 && edge[0] < 255,
            "edge pixel not blended: {edge:?}"
        );
    }

    #[test]
    fn restores_filter_on_error() {
        let mut renderer = make_renderer();

        let buf_size = Size::<i32, Buffer>::from((64, 64));
        let out_size = Size::<i32, Physical>::from((64, 64));
        let mut texture: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, buf_size).unwrap();
        let mut target = renderer.bind(&mut texture).unwrap();

        {
            let mut frame = renderer
                .render(&mut target, out_size, Transform::Normal)
                .unwrap();
            let elem = SamplingRenderElement::new(FailElement { id: Id::new() }, true);
            let dst = Rectangle::from_size(out_size);
            let res = elem.draw(
                &mut frame,
                Rectangle::from_size(Size::from((1., 1.))),
                dst,
                &[dst],
                &[],
                None,
            );
            assert!(res.is_err());
            let _ = frame.finish().unwrap();
        }

        // The failed nearest draw must not leak the filter: an unwrapped draw stays linear.
        let tex_elem = texture_elem(&mut renderer);
        let pixels = render_pixels(&mut renderer, &tex_elem);
        let edge = pixel(&pixels, 31, 32);
        assert!(
            edge[0] > 0 && edge[0] < 255,
            "edge pixel not blended: {edge:?}"
        );
    }
}
