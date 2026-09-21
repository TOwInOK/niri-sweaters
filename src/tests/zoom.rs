use std::time::Duration;

use approx::assert_abs_diff_eq;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::utils::RescaleRenderElement;
use smithay::backend::renderer::element::Element;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::Color32F;
use smithay::output::{Mode, Output};
use smithay::reexports::wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer;
use smithay::reexports::wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Anchor;
use smithay::utils::{Logical, Physical, Point, Rectangle, Scale, Size, Transform};
use wayland_client::protocol::wl_pointer;

use super::client::{ClientId, LayerConfigureProps};
use super::fixture::Fixture;
use super::knit::{assert_llvmpipe, open_window, render_output_rgba};
use crate::layout::zoom::OutputZoomState;
use crate::niri::OutputRenderElements;
use crate::render_helpers::background_effect::RenderParams;
use crate::render_helpers::framebuffer_effect::FramebufferEffect;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::{RenderCtx, RenderTarget};
use crate::ui::zoom_debug;

/// Minimal config for deterministic rendering: no animations, no gaps.
const CONFIG: &str = r#"
animations {
    off
}

layout {
    gaps 0
}
"#;

const ANIMATED_CONFIG: &str = r#"
hotkey-overlay {
    skip-at-startup
}

layout {
    gaps 0
}
"#;

const FRACTIONAL_SCALE_CONFIG: &str = r#"
animations {
    off
}

hotkey-overlay {
    skip-at-startup
}

output "headless-1" {
    scale 1.5
}

layout {
    gaps 0
}
"#;

const ROTATED_CONFIG: &str = r#"
animations {
    off
}

hotkey-overlay {
    skip-at-startup
}

output "headless-1" {
    transform "90"
}

layout {
    gaps 0
}
"#;

fn set_up_with_config(config_text: &str) -> Fixture {
    let config = niri_config::Config::parse_mem(config_text).unwrap();
    let mut f = Fixture::with_config(config);
    f.niri_state().backend.headless().add_renderer().unwrap();
    f.add_output(1, (1920, 720));
    f
}

fn set_up() -> Fixture {
    set_up_with_config(CONFIG)
}

/// Sets the committed zoom level on the output's monitor.
///
/// With `anchor` as the fixed point: `set_level_immediate` keeps the anchor at
/// its displayed position, which at level 1 is the anchor itself, so the focal
/// point becomes `anchor`.
fn set_zoom(f: &mut Fixture, output: &Output, level: f64, anchor: Point<f64, Logical>) {
    let mon = f.niri().layout.monitor_for_output_mut(output).unwrap();
    mon.zoom_mut().set_level_immediate(level, anchor);
}

fn render_elements(
    state: &mut crate::niri::State,
    output: &Output,
    target: RenderTarget,
) -> Vec<OutputRenderElements<GlesRenderer>> {
    state.niri.update_render_elements(Some(output));
    state
        .backend
        .headless()
        .with_primary_renderer(|renderer| {
            let ctx = RenderCtx {
                renderer,
                target,
                xray: None,
            };
            state.niri.render_to_vec(ctx, output, false)
        })
        .expect("no primary renderer")
}

fn is_zoomed<R: NiriRenderer>(elem: &OutputRenderElements<R>) -> bool {
    matches!(
        elem,
        OutputRenderElements::ZoomedMonitor(_)
            | OutputRenderElements::ZoomedRescaledTile(_)
            | OutputRenderElements::ZoomedLayerSurface(_)
            | OutputRenderElements::ZoomedRelocatedLayerSurface(_)
            | OutputRenderElements::ZoomedRelocatedColor(_)
            | OutputRenderElements::ZoomedSolidColor(_)
    )
}

/// Adds a top-anchored layer surface spanning the output width.
///
/// Returns the surface's logical geometry, which is deterministic: the layer
/// is anchored to the top edge with a fixed height.
fn add_top_layer(f: &mut Fixture, id: ClientId, height: u16) -> Rectangle<i32, Logical> {
    let layer = f.client(id).create_layer(None, Layer::Top, "");
    let surface = layer.surface.clone();
    layer.set_configure_props(LayerConfigureProps {
        anchor: Some(Anchor::Left | Anchor::Right | Anchor::Top),
        size: Some((0, u32::from(height))),
        ..Default::default()
    });
    layer.commit();
    f.roundtrip(id);

    let layer = f.client(id).layer(&surface);
    layer.attach_new_buffer();
    layer.set_size(100, 100);
    layer.ack_last_and_commit();
    f.double_roundtrip(id);

    Rectangle::new(Point::from((0, 0)), Size::from((1920, i32::from(height))))
}

#[test]
fn zoom_identity_no_wrappers() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let id = f.add_client();
    open_window(&mut f, id, "zoom-identity", 400, 300, [0xff, 0, 0, 0xff]);
    add_top_layer(&mut f, id, 50);

    let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);
    assert!(!elements.is_empty());
    assert!(
        elements.iter().all(|elem| !is_zoomed(elem)),
        "level 1 must not produce zoomed elements"
    );
}

#[test]
fn zoom_wraps_desktop_scene() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let id = f.add_client();
    open_window(&mut f, id, "zoom-wrap", 400, 300, [0xff, 0, 0, 0xff]);
    add_top_layer(&mut f, id, 50);

    set_zoom(&mut f, &output, 2., Point::from((100., 100.)));

    let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);
    assert!(!elements.is_empty());
    assert!(
        elements.iter().any(is_zoomed),
        "level 2 must wrap desktop scene elements"
    );
    assert!(
        elements
            .iter()
            .any(|e| matches!(e, OutputRenderElements::ZoomedMonitor(_))),
        "workspace content must be zoomed"
    );
    assert!(
        elements
            .iter()
            .any(|e| matches!(e, OutputRenderElements::ZoomedLayerSurface(_))),
        "layer surfaces must be zoomed"
    );
    assert!(
        elements
            .iter()
            .any(|e| matches!(e, OutputRenderElements::ZoomedSolidColor(_))),
        "backdrop must be zoomed"
    );
}

#[test]
fn zoom_geometry_2x() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let id = f.add_client();
    let layer_geo = add_top_layer(&mut f, id, 50);

    set_zoom(&mut f, &output, 2., Point::from((100., 100.)));

    let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);
    let layer = elements
        .iter()
        .find_map(|e| match e {
            OutputRenderElements::ZoomedLayerSurface(elem) => Some(elem),
            _ => None,
        })
        .expect("zoomed layer surface missing");

    // display = focal + (content - focal) * level, in physical pixels.
    let scale = Scale::from(output.current_scale().fractional_scale());
    let expected = Rectangle::new(Point::from((-100, -100)), Size::from((200, 200)));
    assert_eq!(layer.geometry(scale), expected);
    let _ = layer_geo;
}

#[test]
fn zoom_geometry_fractional() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let id = f.add_client();
    add_top_layer(&mut f, id, 50);

    for level in [1.25, 1.5] {
        set_zoom(&mut f, &output, level, Point::from((100., 100.)));

        let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);
        let layer = elements
            .iter()
            .find_map(|e| match e {
                OutputRenderElements::ZoomedLayerSurface(elem) => Some(elem),
                _ => None,
            })
            .expect("zoomed layer surface missing");

        // display = focal + (content - focal) * level.
        let scale = Scale::from(output.current_scale().fractional_scale());
        let expected_w = (100. * level).round() as i32;
        let expected_h = (100. * level).round() as i32;
        let expected_x = (100. - 100. * level).round() as i32;
        let expected_y = (100. - 100. * level).round() as i32;
        assert_eq!(
            layer.geometry(scale),
            Rectangle::new(
                Point::from((expected_x, expected_y)),
                Size::from((expected_w, expected_h)),
            ),
            "level {level}"
        );
    }
}

#[test]
fn zoom_focal_change_moves_geometry() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let id = f.add_client();
    add_top_layer(&mut f, id, 50);

    set_zoom(&mut f, &output, 2., Point::from((100., 100.)));
    let scale = Scale::from(output.current_scale().fractional_scale());

    let geometry = |f: &mut Fixture| {
        let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);
        elements
            .iter()
            .find_map(|e| match e {
                OutputRenderElements::ZoomedLayerSurface(elem) => Some(elem.geometry(scale)),
                _ => None,
            })
            .expect("zoomed layer surface missing")
    };

    let before = geometry(&mut f);

    // Move the focal point: with a zero-size deadzone, a cursor at (0, 0) pulls
    // the viewport to the output corner, i.e. focal (0, 0).
    let mon = f.niri().layout.monitor_for_output_mut(&output).unwrap();
    let changed = mon.update_zoom_focal_for_cursor(
        Point::from((0., 0.)),
        Zoom {
            deadzone_size: 0.,
            ..Default::default()
        },
    );
    assert!(
        changed,
        "focal must move when the cursor leaves the deadzone"
    );

    let after = geometry(&mut f);
    assert_ne!(before, after, "focal change must change render geometry");

    // focal (0, 0): display = content * 2.
    assert_eq!(
        after,
        Rectangle::new(Point::from((0, 0)), Size::from((200, 200)))
    );
}

#[test]
fn zoom_damage_on_level_change() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let id = f.add_client();
    open_window(&mut f, id, "zoom-damage", 400, 300, [0xff, 0, 0, 0xff]);

    let size = output.current_mode().unwrap().size;
    let scale = Scale::from(output.current_scale().fractional_scale());
    let output_geo = Rectangle::from_size(size);
    let mut tracker = OutputDamageTracker::new(size, scale, Transform::Normal);

    let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);
    let (damage, _states) = tracker.damage_output(1, &elements).unwrap();
    assert!(damage.is_some(), "first frame must be damaged");

    let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);
    let (damage, _states) = tracker.damage_output(1, &elements).unwrap();
    assert!(
        damage.is_none(),
        "unchanged frame must not be damaged: {damage:?}"
    );

    set_zoom(&mut f, &output, 2., Point::from((100., 100.)));

    let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);
    let (damage, _states) = tracker.damage_output(1, &elements).unwrap();
    let damage = damage.expect("zoom change must damage the output");
    for point in [
        output_geo.loc,
        output_geo.loc + output_geo.size.downscale(2).to_point() - Point::from((1, 1)),
        output_geo.loc + Point::from((output_geo.size.w - 1, 0)),
        output_geo.loc + Point::from((0, output_geo.size.h - 1)),
        output_geo.loc + output_geo.size.to_point() - Point::from((1, 1)),
    ] {
        assert!(
            damage.iter().any(|d| d.contains(point)),
            "damage must cover {point:?}: {damage:?}"
        );
    }
}

#[test]
fn zoom_damage_on_focal_change() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let id = f.add_client();
    open_window(
        &mut f,
        id,
        "zoom-damage-focal",
        400,
        300,
        [0xff, 0, 0, 0xff],
    );

    let size = output.current_mode().unwrap().size;
    let scale = Scale::from(output.current_scale().fractional_scale());
    let output_geo = Rectangle::from_size(size);
    let mut tracker = OutputDamageTracker::new(size, scale, Transform::Normal);

    set_zoom(&mut f, &output, 2., Point::from((100., 100.)));

    let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);
    let (damage, _states) = tracker.damage_output(1, &elements).unwrap();
    assert!(damage.is_some(), "first frame must be damaged");

    let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);
    let (damage, _states) = tracker.damage_output(1, &elements).unwrap();
    assert!(
        damage.is_none(),
        "unchanged zoomed frame must not be damaged: {damage:?}"
    );

    // Move the focal point to (0, 0).
    let mon = f.niri().layout.monitor_for_output_mut(&output).unwrap();
    assert!(mon.update_zoom_focal_for_cursor(
        Point::from((0., 0.)),
        Zoom {
            deadzone_size: 0.,
            ..Default::default()
        }
    ));
    let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);
    let (damage, _states) = tracker.damage_output(1, &elements).unwrap();
    let damage = damage.expect("focal change must damage the output");
    for point in [
        output_geo.loc,
        output_geo.loc + output_geo.size.downscale(2).to_point() - Point::from((1, 1)),
        output_geo.loc + Point::from((output_geo.size.w - 1, 0)),
        output_geo.loc + Point::from((0, output_geo.size.h - 1)),
        output_geo.loc + output_geo.size.to_point() - Point::from((1, 1)),
    ] {
        assert!(
            damage.iter().any(|d| d.contains(point)),
            "damage must cover {point:?}: {damage:?}"
        );
    }
}

#[test]
fn zoom_applies_to_all_render_targets() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let id = f.add_client();
    open_window(&mut f, id, "zoom-targets", 400, 300, [0xff, 0, 0, 0xff]);

    set_zoom(&mut f, &output, 2., Point::from((100., 100.)));

    for target in [
        RenderTarget::Output,
        RenderTarget::Screencast,
        RenderTarget::ScreenCapture,
    ] {
        let elements = render_elements(f.niri_state(), &output, target);
        assert!(
            elements.iter().any(is_zoomed),
            "target {target:?} must produce zoomed elements"
        );
    }
}

#[test]
fn zoom_framebuffer_effect_forwarding() {
    let effect = FramebufferEffect::new();
    let params = RenderParams {
        geometry: Rectangle::new(Point::from((10., 20.)), Size::from((100., 50.))),
        subregion: None,
        clip: None,
        scale: 1.,
    };
    let elem = effect.render(None, params, None, 0., 1.);
    assert!(elem.is_framebuffer_effect());
}

// --- zoom debug overlay ---

const DEBUG_CONFIG: &str = r#"
animations {
    off
}

hotkey-overlay {
    skip-at-startup
}

zoom {
    deadzone-size 0.5

    debug {
        deadzone true
        focal-point true
    }
}

layout {
    gaps 0
}
"#;

fn set_up_debug() -> Fixture {
    set_up_with_config(DEBUG_CONFIG)
}

/// Solid-color elements with the given color, in render order.
fn debug_solids<R: NiriRenderer>(
    elements: &[OutputRenderElements<R>],
    color: niri_config::Color,
) -> Vec<Rectangle<f64, Logical>> {
    let color = Color32F::from(color.to_array_premul());
    elements
        .iter()
        .filter_map(|elem| match elem {
            OutputRenderElements::SolidColor(e) if e.color() == color => Some(e.geo()),
            _ => None,
        })
        .collect()
}

#[test]
fn zoom_debug_disabled_by_default() {
    let mut f = set_up();
    let output = f.niri_output(1);

    let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);
    for color in [
        zoom_debug::DEADZONE_COLOR,
        zoom_debug::FOCAL_ACTIVE_COLOR,
        zoom_debug::FOCAL_INACTIVE_COLOR,
        zoom_debug::HALO_COLOR,
    ] {
        assert!(
            debug_solids(&elements, color).is_empty(),
            "no debug elements without debug flags"
        );
    }
}

#[test]
fn zoom_debug_deadzone_output() {
    let mut f = set_up_debug();
    let output = f.niri_output(1);

    // The deadzone is visible at 1x: it is a debug view of the tracking
    // region, not of the zoomed state.
    let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);

    let deadzone = OutputZoomState::deadzone_rect(Size::from((1920., 720.)), 0.5);
    assert_eq!(
        debug_solids(&elements, zoom_debug::DEADZONE_COLOR),
        zoom_debug::deadzone_border_rects(deadzone, 2.),
    );

    // The halo covers the deadzone border and the focal crosshair.
    let mut expected = zoom_debug::deadzone_border_rects(deadzone, 4.).to_vec();
    expected.extend(zoom_debug::crosshair_rects(
        Point::from((960., 360.)),
        14.,
        4.,
    ));
    assert_eq!(debug_solids(&elements, zoom_debug::HALO_COLOR), expected);
}

#[test]
fn zoom_debug_deadzone_zero_size() {
    let mut f = set_up_with_config(&DEBUG_CONFIG.replace("deadzone-size 0.5", "deadzone-size 0"));
    let output = f.niri_output(1);

    let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);

    // A zero-area deadzone is drawn as a crosshair on its center.
    let center = Point::from((960., 360.));
    assert_eq!(
        debug_solids(&elements, zoom_debug::DEADZONE_COLOR),
        zoom_debug::crosshair_rects(center, 14., 2.),
    );
}

#[test]
fn zoom_debug_focal_inactive_at_1x() {
    let mut f = set_up_debug();
    let output = f.niri_output(1);

    // The stored focal point is visible at 1x, in the dimmed inactive style.
    let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);
    assert!(!debug_solids(&elements, zoom_debug::FOCAL_INACTIVE_COLOR).is_empty());
    assert!(debug_solids(&elements, zoom_debug::FOCAL_ACTIVE_COLOR).is_empty());
}

#[test]
fn zoom_debug_focal_active_at_2x() {
    let mut f = set_up_debug();
    let output = f.niri_output(1);

    set_zoom(&mut f, &output, 2., Point::from((480., 180.)));

    let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);
    assert!(!debug_solids(&elements, zoom_debug::FOCAL_ACTIVE_COLOR).is_empty());
    assert!(debug_solids(&elements, zoom_debug::FOCAL_INACTIVE_COLOR).is_empty());
}

#[test]
fn zoom_debug_focal_position() {
    let mut f = set_up_debug();
    let output = f.niri_output(1);

    set_zoom(&mut f, &output, 2., Point::from((480., 180.)));
    let focal = zoom_focal(&mut f, &output);

    let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);
    let rects = debug_solids(&elements, zoom_debug::FOCAL_ACTIVE_COLOR);

    // The marker is centered on the focal point in output-local display
    // coordinates: the desktop zoom transform is not applied to it. Elements
    // are pushed topmost-first: the center square, then the crosshair.
    let mut expected = vec![Rectangle::new(
        Point::from((focal.x - 2., focal.y - 2.)),
        Size::from((4., 4.)),
    )];
    expected.extend(zoom_debug::crosshair_rects(focal, 14., 2.));
    assert_eq!(rects, expected);
}

#[test]
fn zoom_debug_only_on_output_target() {
    let mut f = set_up_debug();
    let output = f.niri_output(1);

    for target in [RenderTarget::Screencast, RenderTarget::ScreenCapture] {
        let elements = render_elements(f.niri_state(), &output, target);
        for color in [
            zoom_debug::DEADZONE_COLOR,
            zoom_debug::FOCAL_ACTIVE_COLOR,
            zoom_debug::FOCAL_INACTIVE_COLOR,
            zoom_debug::HALO_COLOR,
        ] {
            assert!(
                debug_solids(&elements, color).is_empty(),
                "debug elements must not reach {target:?}"
            );
        }
    }

    let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);
    assert!(!debug_solids(&elements, zoom_debug::DEADZONE_COLOR).is_empty());
}

#[test]
fn zoom_debug_hidden_by_overview() {
    let mut f = set_up_debug();
    let output = f.niri_output(1);

    set_overview_open(&mut f, &output, true);

    let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);
    assert!(debug_solids(&elements, zoom_debug::DEADZONE_COLOR).is_empty());
    assert!(debug_solids(&elements, zoom_debug::FOCAL_INACTIVE_COLOR).is_empty());
}

#[test]
fn zoom_debug_hidden_by_screenshot_ui() {
    let mut f = set_up_debug();
    let output = f.niri_output(1);

    f.niri_state().open_screenshot_ui(true, None);
    assert!(f.niri().screenshot_ui.is_open());

    let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);
    assert!(debug_solids(&elements, zoom_debug::DEADZONE_COLOR).is_empty());
    assert!(debug_solids(&elements, zoom_debug::FOCAL_INACTIVE_COLOR).is_empty());
}

#[test]
fn zoom_debug_hidden_by_mru() {
    let mut f = set_up_with_config(&format!(
        "{DEBUG_CONFIG}\nrecent-windows {{ open-delay-ms 0; }}"
    ));
    let output = f.niri_output(1);
    let id = f.add_client();
    open_window(&mut f, id, "one", 100, 100, [255, 0, 0, 255]);
    open_window(&mut f, id, "two", 100, 100, [0, 255, 0, 255]);

    open_mru(&mut f);

    let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);
    assert!(debug_solids(&elements, zoom_debug::DEADZONE_COLOR).is_empty());
    assert!(debug_solids(&elements, zoom_debug::FOCAL_INACTIVE_COLOR).is_empty());
}

#[test]
fn zoom_debug_hidden_by_mru_closing() {
    let mut f = set_up_with_config(&format!(
        "{MRU_ANIMATED_CONFIG}\nzoom {{ debug {{ deadzone true; focal-point true; }}; }}"
    ));
    let output = f.niri_output(1);
    let id = f.add_client();
    open_window(&mut f, id, "one", 100, 100, [255, 0, 0, 255]);
    open_window(&mut f, id, "two", 100, 100, [0, 255, 0, 255]);

    freeze_clock(&mut f);
    open_mru(&mut f);

    // The closing animation keeps the UI visually present: the debug overlay
    // stays hidden until the UI is fully closed.
    f.niri().cancel_mru();
    assert!(!f.niri().window_mru_ui.is_open());
    assert!(f.niri().window_mru_ui.is_active());

    let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);
    assert!(debug_solids(&elements, zoom_debug::DEADZONE_COLOR).is_empty());

    advance_clock(&mut f, 5000);
    assert!(!f.niri().window_mru_ui.is_active());

    let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);
    assert!(!debug_solids(&elements, zoom_debug::DEADZONE_COLOR).is_empty());
}

#[test]
fn zoom_debug_config_reload() {
    let mut f = set_up();
    let output = f.niri_output(1);

    let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);
    assert!(debug_solids(&elements, zoom_debug::DEADZONE_COLOR).is_empty());

    reload_with_zoom(&mut f, "zoom { debug { deadzone true; }; }");

    let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);
    assert!(!debug_solids(&elements, zoom_debug::DEADZONE_COLOR).is_empty());
}

#[test]
fn zoom_debug_hold_restore_focal() {
    let mut f = set_up_debug();
    let output = f.niri_output(1);

    // Store a focal point at 1x by zooming to 2x and back.
    set_zoom(&mut f, &output, 2., Point::from((480., 180.)));
    set_zoom(&mut f, &output, 1., Point::from((0., 0.)));
    let stored = zoom_focal(&mut f, &output);

    // A hold-zoom to 2x anchors at the cursor: the marker follows it in the
    // active style.
    f.niri_state().move_cursor(Point::from((1200., 500.)));
    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 2.);
    assert_eq!(zoom_level(&mut f, &output), 2.);

    let held = zoom_focal(&mut f, &output);
    assert_ne!(held, stored);

    let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);
    let rects = debug_solids(&elements, zoom_debug::FOCAL_ACTIVE_COLOR);
    assert!(rects.iter().all(|rect| rect.contains(held)));
    assert!(debug_solids(&elements, zoom_debug::FOCAL_INACTIVE_COLOR).is_empty());

    // Releasing the hold restores the stored 1x state: the marker is back on
    // the stored focal point in the inactive style.
    hold_release(&mut f, trigger);
    assert_eq!(zoom_level(&mut f, &output), 1.);
    assert_eq!(zoom_focal(&mut f, &output), stored);

    let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);
    let rects = debug_solids(&elements, zoom_debug::FOCAL_INACTIVE_COLOR);
    assert!(rects.iter().all(|rect| rect.contains(stored)));
    assert!(debug_solids(&elements, zoom_debug::FOCAL_ACTIVE_COLOR).is_empty());
}

#[test]
fn zoom_debug_multi_output() {
    let mut f = set_up_debug();
    let output1 = f.niri_output(1);
    f.add_output(2, (1280, 1024));
    let output2 = f.niri_output(2);

    set_zoom(&mut f, &output2, 2., Point::from((320., 256.)));

    // Each output gets its own deadzone and focal geometry.
    let elements = render_elements(f.niri_state(), &output1, RenderTarget::Output);
    let deadzone1 = OutputZoomState::deadzone_rect(Size::from((1920., 720.)), 0.5);
    assert_eq!(
        debug_solids(&elements, zoom_debug::DEADZONE_COLOR),
        zoom_debug::deadzone_border_rects(deadzone1, 2.),
    );
    assert!(!debug_solids(&elements, zoom_debug::FOCAL_INACTIVE_COLOR).is_empty());
    assert!(debug_solids(&elements, zoom_debug::FOCAL_ACTIVE_COLOR).is_empty());

    let elements = render_elements(f.niri_state(), &output2, RenderTarget::Output);
    let deadzone2 = OutputZoomState::deadzone_rect(Size::from((1280., 1024.)), 0.5);
    assert_eq!(
        debug_solids(&elements, zoom_debug::DEADZONE_COLOR),
        zoom_debug::deadzone_border_rects(deadzone2, 2.),
    );
    assert!(!debug_solids(&elements, zoom_debug::FOCAL_ACTIVE_COLOR).is_empty());
    assert!(debug_solids(&elements, zoom_debug::FOCAL_INACTIVE_COLOR).is_empty());
}

#[test]
fn zoom_window_pixels_2x() {
    let mut f = set_up();
    assert_llvmpipe(f.niri_state());
    let output = f.niri_output(1);
    let id = f.add_client();
    open_window(
        &mut f,
        id,
        "zoom-pixels",
        400,
        300,
        [0x40404040, 0x80808080, 0xc0c0c0c0, 0xffffffff],
    );

    let (size, pixels) = render_output_rgba(f.niri_state(), &output);
    let before = color_bbox(&pixels, size, [0x40, 0x80, 0xc0]);
    assert!(before.is_some(), "window pixels must be visible at level 1");

    set_zoom(&mut f, &output, 2., Point::from((100., 100.)));

    let (size, pixels) = render_output_rgba(f.niri_state(), &output);
    let after = color_bbox(&pixels, size, [0x40, 0x80, 0xc0]);
    let after = after.expect("window pixels must be visible at level 2");

    let before = before.unwrap().to_f64();
    let zoomed = Rectangle::new(
        Point::from((
            100. + (before.loc.x - 100.) * 2.,
            100. + (before.loc.y - 100.) * 2.,
        )),
        before.size.upscale(2.),
    );
    // The zoomed window extends past the output; only the on-output part is
    // visible, so compare against the clipped rectangle.
    let output_rect = Rectangle::new(Point::from((0., 0.)), size.to_f64());
    let expected = zoomed.intersection(output_rect).unwrap();
    let after = after.to_f64();

    for (actual, expected) in [
        (after.loc.x, expected.loc.x),
        (after.loc.y, expected.loc.y),
        (after.size.w, expected.size.w),
        (after.size.h, expected.size.h),
    ] {
        assert!(
            (actual - expected).abs() <= 2.,
            "zoomed window bbox mismatch: {actual} vs {expected} (before {before:?}, after {after:?})"
        );
    }
}

use niri_config::{
    Action, Bind, FloatOrInt, Key, Modifiers, MruDirection, Trigger, Zoom, ZoomLevelPreset,
};
use smithay::backend::input::Keycode;
use smithay::backend::renderer::element::Kind;
use smithay::input::keyboard::Keysym;
use smithay::wayland::seat::WaylandFocus;

use crate::input::{ZoomHoldTrigger, ZoomPinchRouting};

fn color_bbox(
    pixels: &[u8],
    size: Size<i32, Physical>,
    rgb: [u8; 3],
) -> Option<Rectangle<i32, Physical>> {
    let mut min = Point::<i32, Physical>::from((i32::MAX, i32::MAX));
    let mut max = Point::<i32, Physical>::from((i32::MIN, i32::MIN));
    let mut found = false;

    for (i, px) in pixels.chunks_exact(4).enumerate() {
        let matches = px[..3].iter().zip(rgb).all(|(a, b)| a.abs_diff(b) <= 2);
        if !matches {
            continue;
        }
        found = true;
        let x = (i as i32) % size.w;
        let y = (i as i32) / size.w;
        min.x = min.x.min(x);
        min.y = min.y.min(y);
        max.x = max.x.max(x);
        max.y = max.y.max(y);
    }

    found.then(|| Rectangle::new(min, Size::from((max.x - min.x + 1, max.y - min.y + 1))))
}

fn cursor_hotspot(f: &mut Fixture, output: &Output) -> Point<f64, Logical> {
    let niri = f.niri();
    let scale = output.current_scale().integer_scale();
    match niri.cursor_manager.get_render_cursor(scale) {
        crate::cursor::RenderCursor::Hidden => Point::from((0., 0.)),
        crate::cursor::RenderCursor::Surface { hotspot, .. } => hotspot.to_f64(),
        crate::cursor::RenderCursor::Named { scale, cursor, .. } => {
            let (_, frame) = cursor.frame(niri.start_time.elapsed().as_millis() as u32);
            crate::cursor::XCursor::hotspot(frame)
                .to_logical(scale)
                .to_f64()
        }
    }
}

/// Freezes the compositor clock so that zoom transitions stay mid-flight.
fn freeze_clock(f: &mut Fixture) {
    f.niri().clock.set_rate(0.);
}
/// Advances the frozen clock by `ms` and commits finished animations.
fn advance_clock(f: &mut Fixture, ms: u64) {
    let now = f.niri().clock.now_unadjusted() + Duration::from_millis(ms);
    f.niri().clock.set_rate(1.);
    f.niri().clock.set_unadjusted(now);
    let _ = f.niri().clock.now();
    f.niri().clock.set_rate(0.);
    f.niri().advance_animations();
}

/// Renders the output including the pointer and returns the physical location
/// of every pointer element.
fn pointer_element_locs(
    state: &mut crate::niri::State,
    output: &Output,
) -> Vec<Point<i32, Physical>> {
    state.niri.update_render_elements(Some(output));
    let elements = state
        .backend
        .headless()
        .with_primary_renderer(|renderer| {
            let ctx = RenderCtx {
                renderer,
                target: RenderTarget::Output,
                xray: None,
            };
            state.niri.render_to_vec(ctx, output, true)
        })
        .expect("no primary renderer");

    let scale = Scale::from(output.current_scale().fractional_scale());
    elements
        .iter()
        .filter_map(|e| match e {
            OutputRenderElements::Pointer(elem) => Some(elem.geometry(scale).loc),
            _ => None,
        })
        .collect()
}

/// Sends a relative pointer motion through the virtual pointer protocol.
fn move_pointer(f: &mut Fixture, id: ClientId, dx: f64, dy: f64) {
    let client = f.client(id);
    let manager = client.state.virtual_pointer_manager.as_ref().unwrap();
    let pointer = manager.create_virtual_pointer(None, &client.qh, ());
    pointer.motion(0, dx, dy);
    f.roundtrip(id);
}

fn pointer_location(f: &mut Fixture) -> Point<f64, Logical> {
    f.niri().seat.get_pointer().unwrap().current_location()
}

fn zoom_focal(f: &mut Fixture, output: &Output) -> Point<f64, Logical> {
    f.niri()
        .layout
        .monitor_for_output(output)
        .unwrap()
        .zoom()
        .focal()
}

#[test]
fn zoom_pointer_identity() {
    let mut f = set_up();
    let output = f.niri_output(1);

    f.niri_state().move_cursor(Point::from((150., 100.)));

    let locs = pointer_element_locs(f.niri_state(), &output);
    assert_eq!(locs.len(), 1, "expected one cursor element");
    // The element is drawn at the pointer position minus the cursor hotspot.
    let hotspot = cursor_hotspot(&mut f, &output);
    let scale = Scale::from(output.current_scale().fractional_scale());
    let expected = (Point::from((150., 100.)) - hotspot).to_physical_precise_round(scale);
    assert_eq!(locs[0], expected);
}

#[test]
fn zoom_pointer_visual_2x() {
    let mut f = set_up();
    let output = f.niri_output(1);

    set_zoom(&mut f, &output, 2., Point::from((100., 100.)));
    f.niri_state().move_cursor(Point::from((150., 100.)));

    let locs = pointer_element_locs(f.niri_state(), &output);
    assert_eq!(locs.len(), 1, "expected one cursor element");
    // Tracking clamps the displayed warp to the deadzone edges at (300, 180);
    // the hotspot is subtracted unscaled.
    let hotspot = cursor_hotspot(&mut f, &output);
    let scale = Scale::from(output.current_scale().fractional_scale());
    let expected = (Point::from((300., 180.)) - hotspot).to_physical_precise_round(scale);
    assert_eq!(locs[0], expected);
}

#[test]
fn zoom_relative_motion_scaled() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let id = f.add_client();

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    f.niri_state().move_cursor(Point::from((960., 360.)));

    move_pointer(&mut f, id, 20., 10.);

    // At level 2 the canonical pointer moves by delta / 2.
    assert_eq!(pointer_location(&mut f), Point::from((970., 365.)));
    // The displayed cursor (980, 370) stays inside the deadzone
    // (480..1440, 180..540), so the focal point does not move.
    assert_eq!(zoom_focal(&mut f, &output), Point::from((960., 360.)));
}

#[test]
fn zoom_relative_motion_1x() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let id = f.add_client();

    f.niri_state().move_cursor(Point::from((960., 360.)));

    move_pointer(&mut f, id, 20., 10.);

    assert_eq!(pointer_location(&mut f), Point::from((980., 370.)));
}

#[test]
fn zoom_deadzone_crossing_moves_focal() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let id = f.add_client();

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    f.niri_state().move_cursor(Point::from((960., 360.)));

    // Canonical +300 puts the displayed cursor at the deadzone's right edge;
    // at level 2 that takes a raw delta of 600.
    move_pointer(&mut f, id, 600., 0.);

    assert_eq!(pointer_location(&mut f), Point::from((1260., 360.)));
    // The focal point moves just enough to keep the displayed cursor on the
    // deadzone edge: focal = (level * cursor - edge) / (level - 1).
    assert_eq!(zoom_focal(&mut f, &output), Point::from((1080., 360.)));
}

#[test]
fn zoom_deadzone_boundary_clamps_viewport() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let id = f.add_client();

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    f.niri_state().move_cursor(Point::from((960., 360.)));

    // Canonical +950 would push the viewport past the right output edge, so
    // the focal point clamps at the output edge instead of the deadzone edge.
    move_pointer(&mut f, id, 1900., 0.);

    assert_eq!(pointer_location(&mut f), Point::from((1910., 360.)));
    assert_eq!(zoom_focal(&mut f, &output), Point::from((1920., 360.)));
}

#[test]
fn zoom_lock_clamps_pointer_to_viewport() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let id = f.add_client();

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    {
        let mon = f.niri().layout.monitor_for_output_mut(&output).unwrap();
        mon.zoom_mut().set_locked(true);
    }
    f.niri_state().move_cursor(Point::from((960., 360.)));

    // The viewport is (480..1440, 180..540); the candidate would leave it.
    move_pointer(&mut f, id, 1000., 0.);

    assert_eq!(pointer_location(&mut f), Point::from((1440., 360.)));
    assert_eq!(zoom_focal(&mut f, &output), Point::from((960., 360.)));
}

#[test]
fn zoom_lock_1x_does_not_clamp() {
    let mut f = set_up();
    f.add_output(2, (1920, 720));
    let output = f.niri_output(1);
    let id = f.add_client();

    {
        let mon = f.niri().layout.monitor_for_output_mut(&output).unwrap();
        mon.zoom_mut().set_locked(true);
    }
    f.niri_state().move_cursor(Point::from((1900., 360.)));

    // At level 1 the zoom lock adds no restriction: the pointer crosses onto
    // the second output at x=1920 like normal.
    move_pointer(&mut f, id, 200., 0.);

    assert_eq!(pointer_location(&mut f), Point::from((2100., 360.)));
}

#[test]
fn zoom_hot_corner_display_space() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let id = f.add_client();

    // Content (480, 180) displays at (0, 0) under this transform.
    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    f.niri()
        .layout
        .monitor_for_output_mut(&output)
        .unwrap()
        .zoom_mut()
        .set_locked(true);
    f.niri_state().move_cursor(Point::from((480., 180.)));

    assert!(!f.niri().layout.is_overview_open());

    // A zero-delta motion re-runs the hot-corner check at the current position.
    move_pointer(&mut f, id, 0., 0.);

    assert!(
        f.niri().layout.is_overview_open(),
        "displayed (0, 0) must trigger the top-left hot corner"
    );
    assert!(f.niri().pointer_inside_hot_corner);
}

#[test]
fn zoom_dnd_icon_follows_display_pointer() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let id = f.add_client();

    open_window(&mut f, id, "dnd-icon", 100, 100, [0xff, 0, 0, 0xff]);
    let surface = f
        .niri()
        .layout
        .windows()
        .next()
        .unwrap()
        .1
        .window
        .wl_surface()
        .unwrap()
        .into_owned();

    f.niri().dnd_icon = Some(crate::niri::DndIcon {
        surface,
        offset: Point::from((10, 20)),
    });

    set_zoom(&mut f, &output, 2., Point::from((100., 100.)));
    f.niri_state().move_cursor(Point::from((150., 100.)));

    // The warp tracks the focal point to the deadzone's left/top edges, so the
    // displayed pointer is at (300, 180); the hotspot and DnD offset stay
    // unscaled.
    let hotspot = cursor_hotspot(&mut f, &output);
    let scale = Scale::from(output.current_scale().fractional_scale());
    let cursor_loc = (Point::from((300., 180.)) - hotspot).to_physical_precise_round(scale);
    let dnd_loc =
        (Point::from((300., 180.)) + Point::from((10., 20.))).to_physical_precise_round(scale);

    let mut locs = pointer_element_locs(f.niri_state(), &output);
    locs.sort_by_key(|p| (p.x, p.y));
    let mut expected = vec![cursor_loc, dnd_loc];
    expected.sort_by_key(|p| (p.x, p.y));
    assert_eq!(
        locs, expected,
        "cursor and DnD icon must anchor to the displayed pointer position"
    );
}

/// Sends an absolute pointer motion in the supplied logical display rectangle.
fn move_pointer_absolute(
    f: &mut Fixture,
    id: ClientId,
    position: Point<f64, Logical>,
    extent: Size<i32, Logical>,
    output_index: Option<usize>,
) {
    let client = f.client(id);
    let output = output_index.and_then(|index| client.state.outputs.keys().nth(index).cloned());
    let manager = client.state.virtual_pointer_manager.as_ref().unwrap();
    let pointer = match output.as_ref() {
        Some(output) => {
            manager.create_virtual_pointer_with_output(None, Some(output), &client.qh, ())
        }
        None => manager.create_virtual_pointer(None, &client.qh, ()),
    };
    pointer.motion_absolute(
        0,
        position.x as u32,
        position.y as u32,
        extent.w as u32,
        extent.h as u32,
    );
    f.roundtrip(id);
}

fn global_output_bounds(f: &mut Fixture) -> Rectangle<i32, Logical> {
    let niri = f.niri();
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    for output in niri.global_space.outputs() {
        let geo = niri.global_space.output_geometry(output).unwrap();
        min_x = min_x.min(geo.loc.x);
        min_y = min_y.min(geo.loc.y);
        max_x = max_x.max(geo.loc.x + geo.size.w);
        max_y = max_y.max(geo.loc.y + geo.size.h);
    }
    Rectangle::new(
        Point::from((min_x, min_y)),
        Size::from((max_x - min_x, max_y - min_y)),
    )
}

#[test]
fn zoom_absolute_pointer_identity() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let id = f.add_client();
    let extent = f.niri().global_space.output_geometry(&output).unwrap().size;

    move_pointer_absolute(&mut f, id, Point::from((200., 100.)), extent, None);

    assert_eq!(pointer_location(&mut f), Point::from((200., 100.)));
}

#[test]
fn zoom_absolute_pointer_inverse() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let id = f.add_client();
    let extent = f.niri().global_space.output_geometry(&output).unwrap().size;

    set_zoom(&mut f, &output, 2., Point::from((100., 360.)));
    // Keep the pointer inside the deadzone so the camera does not follow it.
    f.niri_state().move_cursor(Point::from((530., 360.)));
    move_pointer_absolute(&mut f, id, Point::from((600., 360.)), extent, None);

    assert_eq!(pointer_location(&mut f), Point::from((350., 360.)));

    let locs = pointer_element_locs(f.niri_state(), &output);
    assert_eq!(locs.len(), 1, "expected one cursor element");
    let hotspot = cursor_hotspot(&mut f, &output);
    let scale = Scale::from(output.current_scale().fractional_scale());
    let expected = (Point::from((600., 360.)) - hotspot).to_physical_precise_round(scale);
    assert_eq!(locs[0], expected);
}

#[test]
fn zoom_absolute_pointer_fractional_levels() {
    for (level, expected_x) in [(1.25, 1152.), (1.5, 1120.)] {
        let mut f = set_up();
        let output = f.niri_output(1);
        let id = f.add_client();
        let extent = f.niri().global_space.output_geometry(&output).unwrap().size;

        set_zoom(&mut f, &output, level, Point::from((960., 360.)));
        // Keep the pointer inside the deadzone so the camera does not follow it.
        f.niri_state().move_cursor(Point::from((960., 360.)));
        move_pointer_absolute(&mut f, id, Point::from((1200., 360.)), extent, None);

        assert_eq!(pointer_location(&mut f), Point::from((expected_x, 360.)));
    }
}

#[test]
fn zoom_absolute_pointer_fractional_output_scale() {
    let mut f = set_up_with_config(FRACTIONAL_SCALE_CONFIG);
    let output = f.niri_output(1);
    let id = f.add_client();
    let extent = f.niri().global_space.output_geometry(&output).unwrap().size;

    let focal = Point::from((extent.w as f64 / 2., extent.h as f64 / 2.));
    let display = focal + Point::from((200., 0.));
    set_zoom(&mut f, &output, 2., focal);
    // Keep the pointer inside the deadzone so the camera does not follow it.
    f.niri_state().move_cursor(focal);
    move_pointer_absolute(&mut f, id, display, extent, None);

    assert_eq!(pointer_location(&mut f), focal + Point::from((100., 0.)));

    let locs = pointer_element_locs(f.niri_state(), &output);
    assert_eq!(locs.len(), 1, "expected one cursor element");
    let hotspot = cursor_hotspot(&mut f, &output);
    let scale = Scale::from(output.current_scale().fractional_scale());
    let expected = (display - hotspot).to_physical_precise_round(scale);
    assert_eq!(locs[0], expected);
}

#[test]
fn zoom_absolute_pointer_applies_output_transform_before_zoom() {
    let mut f = set_up_with_config(ROTATED_CONFIG);
    let output = f.niri_output(1);
    let id = f.add_client();
    let geo = f.niri().global_space.output_geometry(&output).unwrap();
    let transform = output.current_transform();
    assert_eq!(transform, Transform::_90);

    let focal = Point::from((geo.size.w as f64 / 2., geo.size.h as f64 / 2.));
    let display = focal + Point::from((40., 0.));
    let raw_size = transform.invert().transform_size(geo.size);
    let raw = transform
        .invert()
        .transform_point_in(display, &geo.size.to_f64());
    set_zoom(&mut f, &output, 2., focal);
    // Keep the pointer inside the deadzone so the camera does not follow it.
    f.niri_state().move_cursor(focal);
    move_pointer_absolute(&mut f, id, raw, raw_size, Some(0));

    let actual = pointer_location(&mut f);
    assert_eq!(actual, focal + Point::from((20., 0.)));
}

#[test]
fn zoom_absolute_pointer_deadzone_moves_focal() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let id = f.add_client();
    let extent = f.niri().global_space.output_geometry(&output).unwrap().size;

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    // Keep the pointer inside the deadzone so the camera does not follow it.
    f.niri_state().move_cursor(Point::from((960., 360.)));
    move_pointer_absolute(&mut f, id, Point::from((1560., 360.)), extent, None);

    assert_eq!(pointer_location(&mut f), Point::from((1260., 360.)));
    assert_eq!(zoom_focal(&mut f, &output), Point::from((1080., 360.)));
}

#[test]
fn zoom_absolute_pointer_lock_preserves_focal() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let id = f.add_client();
    let extent = f.niri().global_space.output_geometry(&output).unwrap().size;

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    f.niri()
        .layout
        .monitor_for_output_mut(&output)
        .unwrap()
        .zoom_mut()
        .set_locked(true);

    move_pointer_absolute(&mut f, id, Point::from((1560., 360.)), extent, None);

    assert_eq!(pointer_location(&mut f), Point::from((1260., 360.)));
    assert_eq!(zoom_focal(&mut f, &output), Point::from((960., 360.)));
}

#[test]
fn zoom_absolute_pointer_unmapped_multi_output() {
    let mut f = set_up();
    f.add_output(2, (1920, 720));
    let output1 = f.niri_output(1);
    let output2 = f.niri_output(2);
    let id = f.add_client();
    let bounds = global_output_bounds(&mut f);
    let geo1 = f.niri().global_space.output_geometry(&output1).unwrap();
    let geo2 = f.niri().global_space.output_geometry(&output2).unwrap();

    set_zoom(&mut f, &output1, 2., Point::from((100., 100.)));
    // Keep the pointer inside the deadzone so the camera does not follow it.
    f.niri_state()
        .move_cursor(geo1.loc.to_f64() + Point::from((530., 230.)));

    let display1 = geo1.loc.to_f64() + Point::from((600., 100.));
    move_pointer_absolute(
        &mut f,
        id,
        display1 - bounds.loc.to_f64(),
        bounds.size,
        None,
    );
    assert_eq!(
        pointer_location(&mut f),
        geo1.loc.to_f64() + Point::from((350., 100.))
    );

    let display2 = geo2.loc.to_f64() + Point::from((200., 100.));
    move_pointer_absolute(
        &mut f,
        id,
        display2 - bounds.loc.to_f64(),
        bounds.size,
        None,
    );
    assert_eq!(
        pointer_location(&mut f),
        geo2.loc.to_f64() + Point::from((200., 100.))
    );
}

#[test]
fn zoom_absolute_pointer_explicit_output_mapping() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let id = f.add_client();
    let extent = f.niri().global_space.output_geometry(&output).unwrap().size;

    set_zoom(&mut f, &output, 2., Point::from((100., 360.)));
    // Keep the pointer inside the deadzone so the camera does not follow it.
    f.niri_state().move_cursor(Point::from((530., 360.)));
    move_pointer_absolute(&mut f, id, Point::from((600., 360.)), extent, Some(0));

    assert_eq!(pointer_location(&mut f), Point::from((350., 360.)));
}

#[test]
fn zoom_absolute_pointer_hot_corner_uses_display_space() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let id = f.add_client();
    let extent = f.niri().global_space.output_geometry(&output).unwrap().size;

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    move_pointer_absolute(&mut f, id, Point::from((0., 0.)), extent, None);

    assert!(f.niri().layout.is_overview_open());
    assert!(f.niri().pointer_inside_hot_corner);
}

fn displayed_pointer_location(f: &mut Fixture) -> Point<f64, Logical> {
    let location = pointer_location(f);
    let niri = f.niri();
    let (output, local) = niri
        .output_under(location)
        .expect("pointer must be on output");
    let origin = niri
        .global_space
        .output_geometry(output)
        .unwrap()
        .loc
        .to_f64();
    let transform = niri
        .layout
        .monitor_for_output(output)
        .unwrap()
        .zoom()
        .viewport_transform();
    origin + transform.apply(local)
}

fn silent_warp(f: &mut Fixture, requested: Point<f64, Logical>) -> Point<f64, Logical> {
    let state = f.niri_state();
    let (target, output) = state.prepare_zoom_warp_target(requested);
    state.niri.seat.get_pointer().unwrap().set_location(target);
    state.update_zoom_focal_for_cursor(target, output.as_ref());
    target
}

#[test]
fn zoom_programmatic_warp_identity() {
    let mut f = set_up();
    let target = Point::from((150., 100.));

    f.niri_state().move_cursor(target);

    assert_eq!(pointer_location(&mut f), target);
}

#[test]
fn zoom_programmatic_warp_inside_deadzone_preserves_focal() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let focal = Point::from((960., 360.));
    let target = Point::from((1000., 360.));

    set_zoom(&mut f, &output, 2., focal);
    f.niri_state().move_cursor(target);

    assert_eq!(pointer_location(&mut f), target);
    assert_eq!(zoom_focal(&mut f, &output), focal);
    assert_eq!(
        displayed_pointer_location(&mut f),
        Point::from((1040., 360.))
    );
}

#[test]
fn zoom_programmatic_warp_outside_deadzone_tracks_immediately() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let target = Point::from((1500., 360.));

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    f.niri_state().move_cursor(target);

    assert_eq!(pointer_location(&mut f), target);
    assert_eq!(zoom_focal(&mut f, &output), Point::from((1560., 360.)));
    assert_eq!(
        displayed_pointer_location(&mut f),
        Point::from((1440., 360.))
    );
}

#[test]
fn zoom_programmatic_warp_deadzone_boundary_respects_output_clamp() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let target = Point::from((10., 360.));

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    f.niri_state().move_cursor(target);

    assert_eq!(pointer_location(&mut f), target);
    assert_eq!(zoom_focal(&mut f, &output), Point::from((0., 360.)));
    assert_eq!(displayed_pointer_location(&mut f), Point::from((20., 360.)));
}

#[test]
fn zoom_programmatic_warp_has_no_animation_step() {
    let mut f = set_up();
    let output = f.niri_output(1);

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    f.niri_state().move_cursor(Point::from((1500., 360.)));

    assert_eq!(zoom_focal(&mut f, &output), Point::from((1560., 360.)));
    let locs = pointer_element_locs(f.niri_state(), &output);
    assert_eq!(
        locs.len(),
        1,
        "expected one cursor element after immediate warp"
    );
    assert_eq!(
        displayed_pointer_location(&mut f),
        Point::from((1440., 360.))
    );
}

#[test]
fn zoom_programmatic_warp_locked_inside_viewport_is_exact() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let focal = Point::from((960., 360.));
    let target = Point::from((1000., 360.));

    set_zoom(&mut f, &output, 2., focal);
    f.niri()
        .layout
        .monitor_for_output_mut(&output)
        .unwrap()
        .zoom_mut()
        .set_locked(true);
    f.niri_state().move_cursor(target);

    assert_eq!(pointer_location(&mut f), target);
    assert_eq!(zoom_focal(&mut f, &output), focal);
}

#[test]
fn zoom_programmatic_warp_locked_outside_viewport_clamps_target() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let focal = Point::from((960., 360.));

    set_zoom(&mut f, &output, 2., focal);
    f.niri()
        .layout
        .monitor_for_output_mut(&output)
        .unwrap()
        .zoom_mut()
        .set_locked(true);
    f.niri_state().move_cursor(Point::from((1900., 360.)));

    assert_eq!(pointer_location(&mut f), Point::from((1440., 360.)));
    assert_eq!(zoom_focal(&mut f, &output), focal);
}

#[test]
fn zoom_programmatic_warp_locked_one_x_does_not_clamp() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let target = Point::from((1900., 360.));

    f.niri()
        .layout
        .monitor_for_output_mut(&output)
        .unwrap()
        .zoom_mut()
        .set_locked(true);
    f.niri_state().move_cursor(target);

    assert_eq!(pointer_location(&mut f), target);
}

#[test]
fn zoom_programmatic_warp_cross_output_uses_destination_identity() {
    let mut f = set_up();
    f.add_output(2, (1920, 720));
    let output1 = f.niri_output(1);
    let output2 = f.niri_output(2);
    let target = f
        .niri()
        .global_space
        .output_geometry(&output2)
        .unwrap()
        .loc
        .to_f64()
        + Point::from((200., 100.));

    set_zoom(&mut f, &output1, 2., Point::from((100., 100.)));
    f.niri_state().move_cursor(target);

    assert_eq!(pointer_location(&mut f), target);
    assert_eq!(zoom_focal(&mut f, &output1), Point::from((100., 100.)));
    assert_eq!(zoom_focal(&mut f, &output2), Point::from((960., 360.)));
}

#[test]
fn zoom_programmatic_warp_cross_output_unlocked_tracks_destination() {
    let mut f = set_up();
    f.add_output(2, (1920, 720));
    let output1 = f.niri_output(1);
    let output2 = f.niri_output(2);
    let geo2 = f.niri().global_space.output_geometry(&output2).unwrap();
    let target = geo2.loc.to_f64() + Point::from((1500., 360.));

    set_zoom(&mut f, &output1, 2., Point::from((100., 100.)));
    set_zoom(&mut f, &output2, 2., Point::from((960., 360.)));
    f.niri_state().move_cursor(target);

    assert_eq!(pointer_location(&mut f), target);
    assert_eq!(zoom_focal(&mut f, &output1), Point::from((100., 100.)));
    assert_eq!(zoom_focal(&mut f, &output2), Point::from((1560., 360.)));
    assert_eq!(
        displayed_pointer_location(&mut f),
        geo2.loc.to_f64() + Point::from((1440., 360.))
    );
}

#[test]
fn zoom_programmatic_warp_cross_output_locked_clamps_destination() {
    let mut f = set_up();
    f.add_output(2, (1920, 720));
    let output2 = f.niri_output(2);
    let geo2 = f.niri().global_space.output_geometry(&output2).unwrap();
    let focal = Point::from((960., 360.));

    set_zoom(&mut f, &output2, 2., focal);
    f.niri()
        .layout
        .monitor_for_output_mut(&output2)
        .unwrap()
        .zoom_mut()
        .set_locked(true);
    f.niri_state()
        .move_cursor(geo2.loc.to_f64() + Point::from((1800., 360.)));

    assert_eq!(
        pointer_location(&mut f),
        geo2.loc.to_f64() + Point::from((1440., 360.))
    );
    assert_eq!(zoom_focal(&mut f, &output2), focal);
}

#[test]
fn zoom_tablet_proximity_out_equivalent_warp_uses_zoom_policy() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let focal = Point::from((960., 360.));
    let tablet_target = Point::from((1500., 360.));

    set_zoom(&mut f, &output, 2., focal);
    f.niri().tablet_cursor_location = Some(tablet_target);
    f.niri_state().move_cursor(tablet_target);

    assert_eq!(pointer_location(&mut f), tablet_target);
    assert_eq!(zoom_focal(&mut f, &output), Point::from((1560., 360.)));
}

#[test]
fn zoom_constraint_warp_identity_preserves_set_location_semantics() {
    let mut f = set_up();
    let target = Point::from((150., 100.));

    let final_target = silent_warp(&mut f, target);

    assert_eq!(final_target, target);
    assert_eq!(pointer_location(&mut f), target);
}

#[test]
fn zoom_constraint_warp_unlocked_tracks_focal_without_motion_path() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let target = Point::from((1500., 360.));

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    let final_target = silent_warp(&mut f, target);

    assert_eq!(final_target, target);
    assert_eq!(pointer_location(&mut f), target);
    assert_eq!(zoom_focal(&mut f, &output), Point::from((1560., 360.)));
}

#[test]
fn zoom_constraint_warp_locked_clamps_without_focal_change() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let focal = Point::from((960., 360.));

    set_zoom(&mut f, &output, 2., focal);
    f.niri()
        .layout
        .monitor_for_output_mut(&output)
        .unwrap()
        .zoom_mut()
        .set_locked(true);
    let final_target = silent_warp(&mut f, Point::from((1900., 360.)));

    assert_eq!(final_target, Point::from((1440., 360.)));
    assert_eq!(pointer_location(&mut f), final_target);
    assert_eq!(zoom_focal(&mut f, &output), focal);
}

#[test]
fn zoom_programmatic_warp_preserves_canonical_target_after_tracking() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let target = Point::from((1500., 360.));

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    f.niri_state().move_cursor(target);

    // Focal tracking changes only the viewport; it must not transform the canonical target.
    assert_eq!(pointer_location(&mut f), target);
}

#[test]
fn zoom_programmatic_warp_destination_is_rendered_after_focal_change() {
    let mut f = set_up();
    let output = f.niri_output(1);

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    f.niri_state().move_cursor(Point::from((1500., 360.)));

    let locs = pointer_element_locs(f.niri_state(), &output);
    assert_eq!(locs.len(), 1, "expected destination cursor to be rendered");
    assert_eq!(
        displayed_pointer_location(&mut f),
        Point::from((1440., 360.))
    );
}

#[test]
fn zoom_tablet_cursor_does_not_track_focal() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let focal = Point::from((960., 360.));

    set_zoom(&mut f, &output, 2., focal);
    f.niri().tablet_cursor_location = Some(Point::from((1500., 360.)));
    let _ = pointer_element_locs(f.niri_state(), &output);

    assert_eq!(zoom_focal(&mut f, &output), focal);
}

#[test]
fn zoom_tablet_cursor_visual_2x_uses_canonical_location() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let content = Point::from((150., 100.));

    set_zoom(&mut f, &output, 2., Point::from((100., 100.)));
    f.niri().tablet_cursor_location = Some(content);

    let locs = pointer_element_locs(f.niri_state(), &output);
    assert_eq!(locs.len(), 1, "expected one tablet cursor element");

    let hotspot = cursor_hotspot(&mut f, &output);
    let scale = Scale::from(output.current_scale().fractional_scale());
    let expected = (Point::from((200., 100.)) - hotspot).to_physical_precise_round(scale);
    assert_eq!(locs[0], expected);
    assert_eq!(f.niri().tablet_cursor_location, Some(content));
}

#[test]
fn zoom_tablet_cursor_visual_fractional_levels() {
    for (level, expected_x) in [(1.25, 162.5), (1.5, 175.)] {
        let mut f = set_up();
        let output = f.niri_output(1);
        let content = Point::from((150., 100.));

        set_zoom(&mut f, &output, level, Point::from((100., 100.)));
        f.niri().tablet_cursor_location = Some(content);

        let locs = pointer_element_locs(f.niri_state(), &output);
        assert_eq!(locs.len(), 1, "expected one tablet cursor element");
        let hotspot = cursor_hotspot(&mut f, &output);
        let scale = Scale::from(output.current_scale().fractional_scale());
        let expected = (Point::from((expected_x, 100.)) - hotspot).to_physical_precise_round(scale);
        assert_eq!(locs[0], expected);
    }
}

#[test]
fn zoom_tablet_cursor_locked_render_does_not_move_focal() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let focal = Point::from((960., 360.));
    let content = Point::from((1260., 360.));

    set_zoom(&mut f, &output, 2., focal);
    f.niri()
        .layout
        .monitor_for_output_mut(&output)
        .unwrap()
        .zoom_mut()
        .set_locked(true);
    f.niri().tablet_cursor_location = Some(content);

    let _ = pointer_element_locs(f.niri_state(), &output);

    assert_eq!(zoom_focal(&mut f, &output), focal);
    assert_eq!(f.niri().tablet_cursor_location, Some(content));
}

#[test]
fn zoom_geometry_fractional_output_scale() {
    // The zoom wrapper scales physical geometry around the physical focal
    // point. Check that at a fractional output scale the result matches the
    // ViewportTransform applied in logical space, converted to physical once.
    //
    // RescaleRenderElement rounds element geometry to physical pixels before
    // scaling, so allow a small per-edge deviation.
    use crate::render_helpers::solid_color::{SolidColorBuffer, SolidColorRenderElement};
    use crate::utils::view::ViewportTransform;

    for output_scale in [1.25, 1.5] {
        let scale = Scale::from(output_scale);
        for level in [1.25, 2.] {
            let focal = Point::from((100., 100.));
            let transform = ViewportTransform::new(focal, level);
            let origin = focal.to_physical_precise_round(scale);

            let buffer = SolidColorBuffer::new((50., 30.), [0., 0., 0., 1.]);
            let elem = SolidColorRenderElement::from_buffer(
                &buffer,
                Point::from((40., 60.)),
                1.,
                Kind::Unspecified,
            );
            let zoomed = RescaleRenderElement::from_element(elem.clone(), origin, level);

            // Ideal: transform in logical space, then convert to physical.
            let ideal = transform
                .apply_rect(elem.geo())
                .to_physical_precise_round(scale);
            let actual = zoomed.geometry(scale);

            for (a, b) in [
                (actual.loc.x, ideal.loc.x),
                (actual.loc.y, ideal.loc.y),
                (actual.size.w, ideal.size.w),
                (actual.size.h, ideal.size.h),
            ] {
                let (a, b): (i32, i32) = (a, b);
                assert!(
                    (a - b).abs() <= 2,
                    "scale {output_scale} level {level}: {actual:?} vs ideal {ideal:?}"
                );
            }
        }
    }
}

const EPS: f64 = 1e-9;

fn zoom_level(f: &mut Fixture, output: &Output) -> f64 {
    f.niri()
        .layout
        .monitor_for_output(output)
        .unwrap()
        .zoom()
        .level()
}

fn zoom_target_level(f: &mut Fixture, output: &Output) -> f64 {
    f.niri()
        .layout
        .monitor_for_output(output)
        .unwrap()
        .zoom()
        .target_level()
}

fn zoom_locked(f: &mut Fixture, output: &Output) -> bool {
    f.niri()
        .layout
        .monitor_for_output(output)
        .unwrap()
        .zoom()
        .is_locked()
}

#[test]
fn zoom_action_zoom_in_from_1x() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri_state().do_action(Action::ZoomIn, false);

    assert_abs_diff_eq!(zoom_level(&mut f, &output), 1.2, epsilon = EPS);
    assert_abs_diff_eq!(zoom_target_level(&mut f, &output), 1.2, epsilon = EPS);
}

#[test]
fn zoom_action_repeated_zoom_in() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    for expected in [1.2, 1.44, 1.728] {
        f.niri_state().do_action(Action::ZoomIn, false);
        assert_abs_diff_eq!(zoom_level(&mut f, &output), expected, epsilon = EPS);
        assert_abs_diff_eq!(zoom_target_level(&mut f, &output), expected, epsilon = EPS);
    }
}

#[test]
fn zoom_action_zoom_out() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(1.44)), false);
    f.niri_state().do_action(Action::ZoomOut, false);
    assert_abs_diff_eq!(zoom_level(&mut f, &output), 1.2, epsilon = EPS);

    f.niri_state().do_action(Action::ZoomOut, false);
    assert_eq!(zoom_level(&mut f, &output), 1.);
    assert_eq!(zoom_target_level(&mut f, &output), 1.);
}

#[test]
fn zoom_action_zoom_out_snaps_to_one() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    // A level just above 1 zooms out below the lower bound and clamps to 1.
    set_zoom(&mut f, &output, 1.03, Point::from((150., 100.)));
    f.niri_state().do_action(Action::ZoomOut, false);
    assert_eq!(zoom_level(&mut f, &output), 1.);

    // Floating-point residue from a zoom-in/out round trip snaps to exactly 1.
    for _ in 0..3 {
        f.niri_state().do_action(Action::ZoomIn, false);
    }
    for _ in 0..3 {
        f.niri_state().do_action(Action::ZoomOut, false);
    }
    assert_eq!(zoom_level(&mut f, &output), 1.);
    assert_eq!(zoom_target_level(&mut f, &output), 1.);
}

#[test]
fn zoom_action_locked_zooms_around_viewport_center() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri()
        .layout
        .monitor_for_output_mut(&output)
        .unwrap()
        .zoom_mut()
        .set_locked(true);
    f.niri_state().do_action(Action::ZoomIn, false);

    assert_abs_diff_eq!(zoom_level(&mut f, &output), 1.2, epsilon = EPS);
    let focal = zoom_focal(&mut f, &output);
    assert_abs_diff_eq!(focal.x, 960., epsilon = EPS);
    assert_abs_diff_eq!(focal.y, 360., epsilon = EPS);
}

#[test]
fn zoom_action_locked_keeps_viewport_center_fixed() {
    let mut f = set_up();
    let output = f.niri_output(1);

    // An off-center focal point, as left by a deadzone follow.
    set_zoom(&mut f, &output, 2., Point::from((100., 100.)));
    let center_before = crate::utils::center_f64(
        f.niri()
            .layout
            .monitor_for_output(&output)
            .unwrap()
            .zoom()
            .viewport(),
    );

    f.niri()
        .layout
        .monitor_for_output_mut(&output)
        .unwrap()
        .zoom_mut()
        .set_locked(true);
    f.niri_state().do_action(Action::ZoomIn, false);

    let center_after = crate::utils::center_f64(
        f.niri()
            .layout
            .monitor_for_output(&output)
            .unwrap()
            .zoom()
            .viewport(),
    );
    assert_abs_diff_eq!(center_after.x, center_before.x, epsilon = EPS);
    assert_abs_diff_eq!(center_after.y, center_before.y, epsilon = EPS);
    // The focal point moved to keep the viewport center fixed.
    assert_ne!(zoom_focal(&mut f, &output), Point::from((100., 100.)));
}

#[test]
fn zoom_action_unlocked_zooms_around_pointer() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri_state().do_action(Action::ZoomIn, false);
    // Unlocked, the pointer stays the anchor: the focal point lands on the
    // cursor, not the screen center.
    let focal = zoom_focal(&mut f, &output);
    assert_abs_diff_eq!(focal.x, 150., epsilon = EPS);
    assert_abs_diff_eq!(focal.y, 100., epsilon = EPS);
}

#[test]
fn zoom_action_max_clamp() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    for _ in 0..30 {
        f.niri_state().do_action(Action::ZoomIn, false);
    }

    assert_eq!(zoom_level(&mut f, &output), 10.);
    assert_eq!(zoom_target_level(&mut f, &output), 10.);
}

#[test]
fn zoom_action_set_zoom_level() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(2.)), false);

    assert_eq!(zoom_level(&mut f, &output), 2.);
    assert_eq!(zoom_target_level(&mut f, &output), 2.);
}

#[test]
fn zoom_action_set_zoom_level_invalid() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    for level in [0.5, 0., -1., f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        f.niri_state()
            .do_action(Action::SetZoomLevel(FloatOrInt(level)), false);
        assert_eq!(zoom_level(&mut f, &output), 1.);
        assert_eq!(zoom_target_level(&mut f, &output), 1.);
    }

    // Levels above the maximum are clamped rather than rejected.
    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(20.)), false);
    assert_eq!(zoom_level(&mut f, &output), 10.);
}

#[test]
fn zoom_action_reset_zoom() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    set_zoom(&mut f, &output, 3., Point::from((150., 100.)));
    f.niri_state().do_action(Action::ResetZoom, false);

    assert_eq!(zoom_level(&mut f, &output), 1.);
    assert_eq!(zoom_target_level(&mut f, &output), 1.);
}

#[test]
fn zoom_action_toggle_zoom_lock() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    let focal = zoom_focal(&mut f, &output);

    assert!(!zoom_locked(&mut f, &output));
    f.niri_state().do_action(Action::ZoomLock(false), false);
    assert!(zoom_locked(&mut f, &output));
    f.niri_state().do_action(Action::ZoomLock(false), false);
    assert!(!zoom_locked(&mut f, &output));

    assert_eq!(zoom_level(&mut f, &output), 2.);
    assert_eq!(zoom_focal(&mut f, &output), focal);
}

#[test]
fn zoom_action_cursor_anchor() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    let before = displayed_pointer_location(&mut f);
    f.niri_state().do_action(Action::ZoomIn, false);

    assert_abs_diff_eq!(zoom_level(&mut f, &output), 1.2, epsilon = EPS);
    assert_eq!(displayed_pointer_location(&mut f), before);
}

#[test]
fn zoom_action_boundary_anchor() {
    let mut f = set_up();
    let output = f.niri_output(1);

    // Zoomed to the right edge: the focal point is at the output edge.
    set_zoom(&mut f, &output, 2., Point::from((1920., 360.)));
    f.niri_state().move_cursor(Point::from((1440., 360.)));

    // Zooming out would push the viewport past the output edge, so the clamp
    // wins over preserving the displayed cursor position.
    f.niri_state().do_action(Action::ZoomOut, false);

    let level = zoom_level(&mut f, &output);
    assert_abs_diff_eq!(level, 2. / 1.2, epsilon = EPS);
    let focal = zoom_focal(&mut f, &output);
    assert!(focal.x <= 1920.);
    assert!(focal.y <= 720.);
}

#[test]
fn zoom_action_targets_pointer_output() {
    let mut f = set_up();
    let output1 = f.niri_output(1);
    f.add_output(2, (1920, 720));
    let output2 = f.niri_output(2);

    // Pointer on output 1 while output 2 is active: the action applies to the
    // pointer output and leaves the other output untouched.
    f.niri_focus_output(2);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri_state().do_action(Action::ZoomIn, false);

    assert_abs_diff_eq!(zoom_level(&mut f, &output1), 1.2, epsilon = EPS);
    assert_eq!(zoom_level(&mut f, &output2), 1.);

    // Zooming the second output does not change the first.
    let geo2 = f.niri().global_space.output_geometry(&output2).unwrap();
    f.niri_state()
        .move_cursor(geo2.loc.to_f64() + Point::from((150., 100.)));
    f.niri_state().do_action(Action::ZoomIn, false);

    assert_abs_diff_eq!(zoom_level(&mut f, &output2), 1.2, epsilon = EPS);
    assert_abs_diff_eq!(zoom_level(&mut f, &output1), 1.2, epsilon = EPS);
}

#[test]
fn zoom_action_pointer_off_output_uses_active() {
    let mut f = set_up();
    let output1 = f.niri_output(1);
    f.add_output(2, (1920, 720));
    let output2 = f.niri_output(2);

    f.niri_focus_output(2);

    // Move the pointer into the gap left of the first output so that it is not
    // on any output; the action then applies to the active output.
    f.niri_state().move_cursor(Point::from((-100., -100.)));

    f.niri_state().do_action(Action::ZoomIn, false);

    assert_abs_diff_eq!(zoom_level(&mut f, &output2), 1.2, epsilon = EPS);
    assert_eq!(zoom_level(&mut f, &output1), 1.);
}

fn set_up_with_zoom(zoom: &str) -> Fixture {
    set_up_with_config(&format!("{CONFIG}\n{zoom}"))
}

fn reload_with_zoom(f: &mut Fixture, zoom: &str) {
    let config = niri_config::Config::parse_mem(&format!("{CONFIG}\n{zoom}")).unwrap();
    f.niri_state().reload_config(Ok(config));
}

#[test]
fn zoom_action_custom_increment_factor() {
    let mut f = set_up_with_zoom("zoom { increment-factor 1.5; }");
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    for expected in [1.5, 2.25] {
        f.niri_state().do_action(Action::ZoomIn, false);
        assert_abs_diff_eq!(zoom_level(&mut f, &output), expected, epsilon = EPS);
    }

    for expected in [1.5, 1.] {
        f.niri_state().do_action(Action::ZoomOut, false);
        assert_abs_diff_eq!(zoom_level(&mut f, &output), expected, epsilon = EPS);
    }
}

#[test]
fn zoom_action_custom_max_zoom() {
    let mut f = set_up_with_zoom("zoom { max-zoom 3; }");
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    for _ in 0..10 {
        f.niri_state().do_action(Action::ZoomIn, false);
    }

    assert_eq!(zoom_level(&mut f, &output), 3.);
    assert_eq!(zoom_target_level(&mut f, &output), 3.);
}

#[test]
fn zoom_action_set_level_uses_config_max() {
    let mut f = set_up_with_zoom("zoom { max-zoom 3; }");
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(8.)), false);

    assert_eq!(zoom_level(&mut f, &output), 3.);
    assert_eq!(zoom_target_level(&mut f, &output), 3.);
}

#[test]
fn zoom_custom_deadzone_relative() {
    let mut f = set_up_with_zoom("zoom { deadzone-size 0.25; }");
    let output = f.niri_output(1);
    let id = f.add_client();

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    f.niri_state().move_cursor(Point::from((960., 360.)));

    // The deadzone is (720..1200, 270..450); canonical +300 puts the displayed
    // cursor at its right edge, which at level 2 takes a raw delta of 600.
    move_pointer(&mut f, id, 600., 0.);

    assert_eq!(pointer_location(&mut f), Point::from((1260., 360.)));
    // The focal point moves just enough to keep the displayed cursor on the
    // deadzone edge: focal = (level * cursor - edge) / (level - 1).
    assert_eq!(zoom_focal(&mut f, &output), Point::from((1320., 360.)));
}

#[test]
fn zoom_custom_deadzone_absolute() {
    let mut f = set_up_with_zoom("zoom { deadzone-size 0.25; }");
    let output = f.niri_output(1);
    let id = f.add_client();
    let extent = f.niri().global_space.output_geometry(&output).unwrap().size;

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    f.niri_state().move_cursor(Point::from((960., 360.)));

    // Display (1200, 360) is the right edge of the 0.25 deadzone; the
    // canonical pointer lands at (1080, 360).
    move_pointer_absolute(&mut f, id, Point::from((1200., 360.)), extent, None);

    assert_eq!(pointer_location(&mut f), Point::from((1080., 360.)));
    assert_eq!(zoom_focal(&mut f, &output), Point::from((960., 360.)));
}

#[test]
fn zoom_custom_deadzone_warp() {
    let mut f = set_up_with_zoom("zoom { deadzone-size 0.25; }");
    let output = f.niri_output(1);

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));

    // Canonical (1080, 360) displays at the right edge of the 0.25 deadzone.
    f.niri_state().move_cursor(Point::from((1080., 360.)));

    assert_eq!(pointer_location(&mut f), Point::from((1080., 360.)));
    assert_eq!(zoom_focal(&mut f, &output), Point::from((960., 360.)));
}

#[test]
fn zoom_reload_lower_max_clamps() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    set_zoom(&mut f, &output, 6., Point::from((150., 100.)));
    let displayed = displayed_pointer_location(&mut f);

    reload_with_zoom(&mut f, "zoom { max-zoom 3; }");

    assert_eq!(zoom_level(&mut f, &output), 3.);
    assert_eq!(zoom_target_level(&mut f, &output), 3.);
    // The pointer stays at its displayed position through the clamp.
    assert_eq!(displayed_pointer_location(&mut f), displayed);
}

#[test]
fn zoom_reload_higher_max_keeps_level() {
    let mut f = set_up_with_zoom("zoom { max-zoom 3; }");
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    set_zoom(&mut f, &output, 3., Point::from((150., 100.)));

    reload_with_zoom(&mut f, "zoom { max-zoom 10; }");

    assert_eq!(zoom_level(&mut f, &output), 3.);
    assert_eq!(zoom_target_level(&mut f, &output), 3.);
}

#[test]
fn zoom_reload_deadzone_does_not_move_focal() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let id = f.add_client();

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    f.niri_state().move_cursor(Point::from((960., 360.)));

    reload_with_zoom(&mut f, "zoom { deadzone-size 0.25; }");

    // The reload itself must not move the focal point.
    assert_eq!(zoom_focal(&mut f, &output), Point::from((960., 360.)));

    // The next tracking event uses the new deadzone: canonical +300 displays
    // at the right edge of the 0.25 box.
    move_pointer(&mut f, id, 600., 0.);
    assert_eq!(zoom_focal(&mut f, &output), Point::from((1320., 360.)));
}

#[test]
fn zoom_reload_increment_factor() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    set_zoom(&mut f, &output, 2., Point::from((150., 100.)));

    reload_with_zoom(&mut f, "zoom { increment-factor 1.5; }");

    // The reload does not change the current zoom state.
    assert_eq!(zoom_level(&mut f, &output), 2.);

    f.niri_state().do_action(Action::ZoomIn, false);
    assert_eq!(zoom_level(&mut f, &output), 3.);
}

#[test]
fn zoom_reload_max_clamps_locked() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    set_zoom(&mut f, &output, 6., Point::from((150., 100.)));
    f.niri()
        .layout
        .monitor_for_output_mut(&output)
        .unwrap()
        .zoom_mut()
        .set_locked(true);

    reload_with_zoom(&mut f, "zoom { max-zoom 3; }");

    assert_eq!(zoom_level(&mut f, &output), 3.);
    assert!(zoom_locked(&mut f, &output));
}

#[test]
fn zoom_reload_max_clamps_per_output() {
    let mut f = set_up();
    let output1 = f.niri_output(1);
    f.add_output(2, (1920, 720));
    let output2 = f.niri_output(2);

    set_zoom(&mut f, &output1, 6., Point::from((150., 100.)));
    set_zoom(&mut f, &output2, 2., Point::from((960., 360.)));

    reload_with_zoom(&mut f, "zoom { max-zoom 3; }");

    assert_eq!(zoom_level(&mut f, &output1), 3.);
    assert_eq!(zoom_level(&mut f, &output2), 2.);
}

fn reload_with_animated(f: &mut Fixture, extra: &str) {
    let config = niri_config::Config::parse_mem(&format!("{ANIMATED_CONFIG}\n{extra}")).unwrap();
    f.niri_state().reload_config(Ok(config));
}

// --- toggle-zoom / hold-zoom ---

/// Simulates a `hold-zoom` bind press: the resolved bind is dispatched with
/// the physical trigger identity, exactly as the input handlers do.
fn hold_press(f: &mut Fixture, trigger: ZoomHoldTrigger, level: f64) {
    hold_press_locked(f, trigger, level, false);
}

/// Simulates a `hold-zoom hold=true` bind press.
fn hold_press_locked(f: &mut Fixture, trigger: ZoomHoldTrigger, level: f64, hold: bool) {
    let bind = Bind {
        key: Key {
            trigger: Trigger::Keysym(Keysym::x),
            modifiers: Modifiers::COMPOSITOR,
        },
        action: Action::HoldZoom(ZoomLevelPreset(level), hold),
        repeat: true,
        cooldown: None,
        allow_when_locked: false,
        allow_inhibiting: true,
        hotkey_overlay_title: None,
    };
    f.niri_state().handle_bind(bind, Some(trigger));
}

/// Simulates the release of the hold trigger.
fn hold_release(f: &mut Fixture, trigger: ZoomHoldTrigger) {
    f.niri_state().end_zoom_hold_for_trigger(trigger);
}

/// Simulates a `zoom-lock hold=true` bind press.
fn zoom_lock_press(f: &mut Fixture, trigger: ZoomHoldTrigger) {
    let bind = Bind {
        key: Key {
            trigger: Trigger::Keysym(Keysym::x),
            modifiers: Modifiers::COMPOSITOR,
        },
        action: Action::ZoomLock(true),
        repeat: true,
        cooldown: None,
        allow_when_locked: false,
        allow_inhibiting: true,
        hotkey_overlay_title: None,
    };
    f.niri_state().handle_bind(bind, Some(trigger));
}

fn key_trigger(code: u32) -> ZoomHoldTrigger {
    ZoomHoldTrigger::Key(Keycode::from(code))
}

#[test]
fn zoom_action_toggle_zoom() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri_state()
        .do_action(Action::ToggleZoom(ZoomLevelPreset(2.), false), false);
    assert_eq!(zoom_level(&mut f, &output), 2.);
    assert_eq!(zoom_target_level(&mut f, &output), 2.);

    f.niri_state()
        .do_action(Action::ToggleZoom(ZoomLevelPreset(2.), false), false);
    assert_eq!(zoom_level(&mut f, &output), 1.);
    assert_eq!(zoom_target_level(&mut f, &output), 1.);
}

#[test]
fn zoom_action_toggle_zoom_after_manual_zoom() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri_state()
        .do_action(Action::ToggleZoom(ZoomLevelPreset(2.), false), false);
    assert_eq!(zoom_level(&mut f, &output), 2.);

    f.niri_state().do_action(Action::ZoomIn, false);
    assert_abs_diff_eq!(zoom_level(&mut f, &output), 2.4, epsilon = EPS);

    // The preset is an entry level, not a pinned session: toggling off any
    // active zoom returns to 1.
    f.niri_state()
        .do_action(Action::ToggleZoom(ZoomLevelPreset(2.), false), false);
    assert_eq!(zoom_level(&mut f, &output), 1.);

    f.niri_state()
        .do_action(Action::ToggleZoom(ZoomLevelPreset(2.), false), false);
    assert_eq!(zoom_level(&mut f, &output), 2.);
}

#[test]
fn zoom_action_toggle_zoom_uses_target_level() {
    let mut f = set_up_with_config(ANIMATED_CONFIG);
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));
    freeze_clock(&mut f);

    // A mid-transition state: the displayed level is above 1 while the
    // user-requested target is already back at 1. The toggle decision must
    // follow the target, so it applies the preset rather than resetting.
    set_zoom(&mut f, &output, 2., Point::from((150., 100.)));
    f.niri_state().do_action(Action::ResetZoom, false);
    assert_eq!(zoom_target_level(&mut f, &output), 1.);

    f.niri_state()
        .do_action(Action::ToggleZoom(ZoomLevelPreset(3.), false), false);
    assert_eq!(zoom_target_level(&mut f, &output), 3.);

    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 3.);
}

#[test]
fn zoom_action_toggle_zoom_clamps_to_max() {
    let mut f = set_up_with_zoom("zoom { max-zoom 3; }");
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri_state()
        .do_action(Action::ToggleZoom(ZoomLevelPreset(8.), false), false);
    assert_eq!(zoom_level(&mut f, &output), 3.);
    assert_eq!(zoom_target_level(&mut f, &output), 3.);
}

#[test]
fn zoom_action_toggle_zoom_targets_pointer_output() {
    let mut f = set_up();
    let output1 = f.niri_output(1);
    f.add_output(2, (1920, 720));
    let output2 = f.niri_output(2);

    // Pointer on output 1 while output 2 is active: the toggle applies to the
    // pointer output.
    f.niri_focus_output(2);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri_state()
        .do_action(Action::ToggleZoom(ZoomLevelPreset(2.), false), false);

    assert_eq!(zoom_level(&mut f, &output1), 2.);
    assert_eq!(zoom_level(&mut f, &output2), 1.);
}

#[test]
fn zoom_action_toggle_zoom_ipc_parse() {
    use clap::Parser;

    let action = niri_ipc::Action::try_parse_from(["niri msg action", "toggle-zoom", "2.0"])
        .expect("toggle-zoom must parse from CLI");
    let niri_ipc::Action::ToggleZoom { level, hold } = action else {
        panic!("expected ToggleZoom, got {action:?}");
    };
    assert_eq!(level, 2.);
    assert!(!hold);

    // The IPC action converts into the config action.
    let action = Action::from(niri_ipc::Action::ToggleZoom {
        level: 2.5,
        hold: false,
    });
    assert_eq!(action, Action::ToggleZoom(ZoomLevelPreset(2.5), false));
}

#[test]
fn zoom_hold_press_and_release() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 2.);
    assert_eq!(zoom_level(&mut f, &output), 2.);
    assert!(f.niri().zoom_hold.is_some());

    hold_release(&mut f, trigger);
    assert_eq!(zoom_level(&mut f, &output), 1.);
    assert_eq!(zoom_target_level(&mut f, &output), 1.);
    assert!(f.niri().zoom_hold.is_none());
}

#[test]
fn zoom_hold_restores_non_one_state() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    set_zoom(&mut f, &output, 1.5, Point::from((150., 100.)));

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 3.);
    assert_eq!(zoom_level(&mut f, &output), 3.);

    hold_release(&mut f, trigger);
    assert_eq!(zoom_level(&mut f, &output), 1.5);
    assert_eq!(zoom_target_level(&mut f, &output), 1.5);
}

#[test]
fn zoom_hold_restores_focal() {
    let mut f = set_up();
    let output = f.niri_output(1);

    // Establish a non-center focal point before the hold.
    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    f.niri_state().move_cursor(Point::from((1500., 360.)));
    let focal = zoom_focal(&mut f, &output);
    assert_ne!(focal, Point::from((960., 360.)));

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 3.);
    // The hold changes level and focal.
    assert_eq!(zoom_level(&mut f, &output), 3.);

    hold_release(&mut f, trigger);
    assert_eq!(zoom_level(&mut f, &output), 2.);
    assert_eq!(zoom_focal(&mut f, &output), focal);
}

#[test]
fn zoom_hold_does_not_restore_lock() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 2.);

    // Locking during the hold is a user preference change, not part of the
    // temporary viewport override.
    f.niri_state().do_action(Action::ZoomLock(false), false);
    assert!(zoom_locked(&mut f, &output));

    hold_release(&mut f, trigger);
    assert_eq!(zoom_level(&mut f, &output), 1.);
    assert!(zoom_locked(&mut f, &output));
}

#[test]
fn zoom_hold_actions_during_hold_then_restore() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    set_zoom(&mut f, &output, 1.5, Point::from((150., 100.)));
    let focal = zoom_focal(&mut f, &output);

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 2.);
    assert_eq!(zoom_level(&mut f, &output), 2.);

    // Regular zoom actions keep working during the hold.
    f.niri_state().do_action(Action::ZoomIn, false);
    f.niri_state().do_action(Action::ZoomIn, false);
    assert_abs_diff_eq!(zoom_level(&mut f, &output), 2.88, epsilon = EPS);

    // Release restores the exact pre-hold state.
    hold_release(&mut f, trigger);
    assert_eq!(zoom_level(&mut f, &output), 1.5);
    assert_eq!(zoom_target_level(&mut f, &output), 1.5);
    assert_eq!(zoom_focal(&mut f, &output), focal);
}

#[test]
fn zoom_hold_release_restores_original_output() {
    let mut f = set_up();
    let output1 = f.niri_output(1);
    f.add_output(2, (1920, 720));
    let output2 = f.niri_output(2);

    // Press on output 1, then move the cursor to output 2.
    f.niri_state().move_cursor(Point::from((150., 100.)));
    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 2.);
    assert_eq!(zoom_level(&mut f, &output1), 2.);

    let geo2 = f.niri().global_space.output_geometry(&output2).unwrap();
    f.niri_state()
        .move_cursor(geo2.loc.to_f64() + Point::from((150., 100.)));

    // Release restores the output fixed at press, not the one under the
    // cursor.
    hold_release(&mut f, trigger);
    assert_eq!(zoom_level(&mut f, &output1), 1.);
    assert_eq!(zoom_level(&mut f, &output2), 1.);
}

#[test]
fn zoom_hold_reentry_replaces() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    set_zoom(&mut f, &output, 1.5, Point::from((150., 100.)));

    let trigger_a = key_trigger(30);
    let trigger_b = key_trigger(48);

    hold_press(&mut f, trigger_a, 2.);
    assert_eq!(zoom_level(&mut f, &output), 2.);

    // A second hold press replaces the active session: it restores the
    // previous state first, then snapshots that restored state.
    hold_press(&mut f, trigger_b, 4.);
    assert_eq!(zoom_level(&mut f, &output), 4.);

    // Releasing the replaced trigger is a no-op.
    hold_release(&mut f, trigger_a);
    assert_eq!(zoom_level(&mut f, &output), 4.);
    assert!(f.niri().zoom_hold.is_some());

    // Releasing the owner restores the original pre-hold state.
    hold_release(&mut f, trigger_b);
    assert_eq!(zoom_level(&mut f, &output), 1.5);
    assert!(f.niri().zoom_hold.is_none());
}

#[test]
fn zoom_hold_ends_on_trigger_key_only() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 2.);

    // Releasing a different key (e.g. a modifier) keeps the hold active.
    hold_release(&mut f, key_trigger(125));
    assert_eq!(zoom_level(&mut f, &output), 2.);
    assert!(f.niri().zoom_hold.is_some());

    // Releasing the trigger key ends the hold.
    hold_release(&mut f, trigger);
    assert_eq!(zoom_level(&mut f, &output), 1.);
    assert!(f.niri().zoom_hold.is_none());
}

#[test]
fn zoom_hold_survives_config_reload() {
    let mut f = set_up_with_config(
        r#"
        animations {
            off
        }
        hotkey-overlay {
            skip-at-startup
        }
        binds {
            Mod+X { hold-zoom 2.0; }
        }
        "#,
    );
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    set_zoom(&mut f, &output, 1.5, Point::from((150., 100.)));

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 2.);
    assert_eq!(zoom_level(&mut f, &output), 2.);

    // Reload with the hold bind removed entirely; the session is owned by the
    // physical trigger, not by the bind table.
    reload_with_zoom(&mut f, "");
    assert!(f.niri().zoom_hold.is_some());

    hold_release(&mut f, trigger);
    assert_eq!(zoom_level(&mut f, &output), 1.5);
    assert!(f.niri().zoom_hold.is_none());
}

#[test]
fn zoom_hold_restore_clamps_to_current_max() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    // Pre-hold state above the future maximum.
    set_zoom(&mut f, &output, 6., Point::from((150., 100.)));

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 2.);
    assert_eq!(zoom_level(&mut f, &output), 2.);

    // Lowering max-zoom during the hold clamps the current level right away
    // and must also clamp the restored state on release.
    reload_with_zoom(&mut f, "zoom { max-zoom 3; }");
    assert_eq!(zoom_level(&mut f, &output), 2.);

    hold_release(&mut f, trigger);
    assert_eq!(zoom_level(&mut f, &output), 3.);
    assert_eq!(zoom_target_level(&mut f, &output), 3.);
}

#[test]
fn zoom_hold_is_not_repeatable() {
    let mut f = set_up();

    let bind = Bind {
        key: Key {
            trigger: Trigger::Keysym(Keysym::x),
            modifiers: Modifiers::COMPOSITOR,
        },
        action: Action::HoldZoom(ZoomLevelPreset(2.), false),
        // Even with the default repeatable flag, hold-zoom must not arm the
        // key repeat timer.
        repeat: true,
        cooldown: None,
        allow_when_locked: false,
        allow_inhibiting: true,
        hotkey_overlay_title: None,
    };
    f.niri_state().start_key_repeat(bind.clone());
    assert!(f.niri().bind_repeat_timer.is_none());

    // A regular repeatable action still arms the timer.
    let bind = Bind {
        action: Action::ZoomIn,
        ..bind
    };
    f.niri_state().start_key_repeat(bind);
    assert!(f.niri().bind_repeat_timer.is_some());

    if let Some(token) = f.niri().bind_repeat_timer.take() {
        f.niri().event_loop.remove(token);
    }
}

#[test]
fn zoom_hold_press_on_cooldown_starts_no_session() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    let bind = Bind {
        key: Key {
            trigger: Trigger::Keysym(Keysym::x),
            modifiers: Modifiers::COMPOSITOR,
        },
        action: Action::HoldZoom(ZoomLevelPreset(2.), false),
        repeat: true,
        cooldown: Some(Duration::from_secs(60)),
        allow_when_locked: false,
        allow_inhibiting: true,
        hotkey_overlay_title: None,
    };

    let trigger_a = key_trigger(30);
    let trigger_b = key_trigger(48);

    // The first press executes and starts the cooldown.
    f.niri_state().handle_bind(bind.clone(), Some(trigger_a));
    assert_eq!(zoom_level(&mut f, &output), 2.);

    // A second press of the same bind is suppressed by the cooldown: no new
    // session replaces the active one.
    f.niri_state().handle_bind(bind, Some(trigger_b));
    assert_eq!(zoom_level(&mut f, &output), 2.);
    assert_eq!(
        f.niri().zoom_hold.as_ref().unwrap().trigger,
        trigger_a,
        "the suppressed press must not replace the active session"
    );

    // The suppressed trigger's release is a no-op; the owner release restores.
    hold_release(&mut f, trigger_b);
    assert_eq!(zoom_level(&mut f, &output), 2.);
    hold_release(&mut f, trigger_a);
    assert_eq!(zoom_level(&mut f, &output), 1.);
}

#[test]
fn zoom_hold_release_not_gated_by_screenshot_ui() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    set_zoom(&mut f, &output, 1.5, Point::from((150., 100.)));

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 2.);

    // Open the screenshot UI during the hold; the release is a cleanup path,
    // not a new action, so it must not be blocked by the UI allow-list.
    f.niri_state().open_screenshot_ui(true, None);
    assert!(f.niri().screenshot_ui.is_open());

    hold_release(&mut f, trigger);
    assert_eq!(zoom_level(&mut f, &output), 1.5);
    assert!(f.niri().zoom_hold.is_none());
}

#[test]
fn zoom_hold_toggle_and_reset_during_hold_then_restore() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    set_zoom(&mut f, &output, 1.5, Point::from((150., 100.)));
    let focal = zoom_focal(&mut f, &output);

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 3.);
    assert_eq!(zoom_level(&mut f, &output), 3.);

    // toggle-zoom during a hold does not end the session; it changes the
    // hold-overridden zoom like a regular action.
    f.niri_state()
        .do_action(Action::ToggleZoom(ZoomLevelPreset(2.), false), false);
    assert_eq!(zoom_level(&mut f, &output), 1.);
    assert!(f.niri().zoom_hold.is_some());

    f.niri_state()
        .do_action(Action::ToggleZoom(ZoomLevelPreset(2.), false), false);
    assert_eq!(zoom_level(&mut f, &output), 2.);

    // reset-zoom during a hold is momentary too.
    f.niri_state().do_action(Action::ResetZoom, false);
    assert_eq!(zoom_level(&mut f, &output), 1.);

    hold_release(&mut f, trigger);
    assert_eq!(zoom_level(&mut f, &output), 1.5);
    assert_eq!(zoom_target_level(&mut f, &output), 1.5);
    assert_eq!(zoom_focal(&mut f, &output), focal);
}

#[test]
fn zoom_hold_pointer_button() {
    use wayland_client::protocol::wl_pointer;

    let mut f = set_up_with_config(
        r#"
        animations {
            off
        }
        hotkey-overlay {
            skip-at-startup
        }
        binds {
            MouseBack { hold-zoom 2.0; }
        }
        "#,
    );
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    let id = f.add_client();
    let pointer = {
        let client = f.client(id);
        let manager = client.state.virtual_pointer_manager.as_ref().unwrap();
        manager.create_virtual_pointer(None, &client.qh, ())
    };

    const BTN_BACK: u32 = 0x116;

    pointer.button(0, BTN_BACK, wl_pointer::ButtonState::Pressed.into());
    f.roundtrip(id);
    assert_eq!(zoom_level(&mut f, &output), 2.);
    assert!(f.niri().zoom_hold.is_some());

    pointer.button(0, BTN_BACK, wl_pointer::ButtonState::Released.into());
    f.roundtrip(id);
    assert_eq!(zoom_level(&mut f, &output), 1.);
    assert!(f.niri().zoom_hold.is_none());
}

#[test]
fn zoom_hold_tablet_button() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    // The fixture cannot inject tablet events; exercise the same production
    // path the tablet handler uses.
    let trigger = ZoomHoldTrigger::TabletButton(0x14b);
    hold_press(&mut f, trigger, 2.);
    assert_eq!(zoom_level(&mut f, &output), 2.);

    hold_release(&mut f, trigger);
    assert_eq!(zoom_level(&mut f, &output), 1.);
    assert!(f.niri().zoom_hold.is_none());
}

#[test]
fn zoom_hold_ends_on_vt_switch() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    set_zoom(&mut f, &output, 1.5, Point::from((150., 100.)));

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 2.);

    // A VT switch may never deliver the trigger release; the hold must end
    // with the rest of the suppressed-key state.
    f.niri_state().do_action(Action::ChangeVt(2), false);
    assert_eq!(zoom_level(&mut f, &output), 1.5);
    assert!(f.niri().zoom_hold.is_none());
}

#[test]
fn zoom_hold_ends_on_suspend() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    set_zoom(&mut f, &output, 1.5, Point::from((150., 100.)));

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 2.);

    f.niri_state().do_action(Action::Suspend, false);
    assert_eq!(zoom_level(&mut f, &output), 1.5);
    assert!(f.niri().zoom_hold.is_none());
}

#[test]
fn zoom_hold_cleanup_restores_state() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    set_zoom(&mut f, &output, 1.5, Point::from((150., 100.)));

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 2.);

    // The shared cleanup path used by device removal: the session does not
    // track which device its trigger came from, so any removal ends it.
    f.niri_state().end_zoom_hold_animated();
    assert_eq!(zoom_level(&mut f, &output), 1.5);
    assert!(f.niri().zoom_hold.is_none());
}

#[test]
fn zoom_hold_output_removal_drops_session() {
    let mut f = set_up();
    let output1 = f.niri_output(1);
    f.add_output(2, (1920, 720));
    let output2 = f.niri_output(2);

    f.niri_state().move_cursor(Point::from((150., 100.)));
    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 2.);
    assert_eq!(zoom_level(&mut f, &output1), 2.);

    // Removing the owning output drops the session without a panic and
    // without moving the snapshot to another output.
    f.niri().remove_output(&output1);
    assert!(f.niri().zoom_hold.is_none());
    assert_eq!(zoom_level(&mut f, &output2), 1.);
}

#[test]
fn zoom_hold_release_without_session_is_noop() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    hold_release(&mut f, key_trigger(30));
    assert_eq!(zoom_level(&mut f, &output), 1.);
}

#[test]
fn zoom_toggle_locks_and_unlocks() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri_state()
        .do_action(Action::ToggleZoom(ZoomLevelPreset(2.), true), false);
    assert_eq!(zoom_level(&mut f, &output), 2.);
    assert!(zoom_locked(&mut f, &output));

    f.niri_state()
        .do_action(Action::ToggleZoom(ZoomLevelPreset(2.), true), false);
    assert_eq!(zoom_level(&mut f, &output), 1.);
    assert!(!zoom_locked(&mut f, &output));
}

#[test]
fn zoom_hold_lock_restores_lock_state() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    let trigger = key_trigger(30);
    hold_press_locked(&mut f, trigger, 2., true);
    assert_eq!(zoom_level(&mut f, &output), 2.);
    assert!(zoom_locked(&mut f, &output));

    hold_release(&mut f, trigger);
    assert_eq!(zoom_level(&mut f, &output), 1.);
    assert!(!zoom_locked(&mut f, &output));
}

#[test]
fn zoom_hold_lock_preserves_prior_lock() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    set_zoom(&mut f, &output, 1.5, Point::from((150., 100.)));
    f.niri_state().do_action(Action::ZoomLock(false), false);
    assert!(zoom_locked(&mut f, &output));

    let trigger = key_trigger(30);
    hold_press_locked(&mut f, trigger, 2., true);
    assert_eq!(zoom_level(&mut f, &output), 2.);
    assert!(zoom_locked(&mut f, &output));

    hold_release(&mut f, trigger);
    assert_eq!(zoom_level(&mut f, &output), 1.5);
    assert!(zoom_locked(&mut f, &output));
}

#[test]
fn zoom_lock_hold_momentary() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    let trigger = key_trigger(30);
    zoom_lock_press(&mut f, trigger);
    assert!(zoom_locked(&mut f, &output));
    assert!(f.niri().zoom_lock_hold.is_some());

    f.niri_state().end_zoom_lock_hold_for_trigger(trigger);
    assert!(!zoom_locked(&mut f, &output));
    assert!(f.niri().zoom_lock_hold.is_none());
}

#[test]
fn zoom_lock_hold_inverts_existing_lock() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri_state().do_action(Action::ZoomLock(false), false);
    assert!(zoom_locked(&mut f, &output));

    let trigger = key_trigger(30);
    zoom_lock_press(&mut f, trigger);
    assert!(!zoom_locked(&mut f, &output));

    f.niri_state().end_zoom_lock_hold_for_trigger(trigger);
    assert!(zoom_locked(&mut f, &output));
}

#[test]
fn zoom_lock_hold_ends_on_vt_switch() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    let trigger = key_trigger(30);
    zoom_lock_press(&mut f, trigger);
    assert!(zoom_locked(&mut f, &output));

    f.niri_state().do_action(Action::ChangeVt(2), false);
    assert!(!zoom_locked(&mut f, &output));
    assert!(f.niri().zoom_lock_hold.is_none());
}

#[test]
fn zoom_lock_hold_output_removal_drops_session() {
    let mut f = set_up();
    let output1 = f.niri_output(1);
    f.add_output(2, (1920, 720));
    let output2 = f.niri_output(2);

    f.niri_state().move_cursor(Point::from((150., 100.)));
    let trigger = key_trigger(30);
    zoom_lock_press(&mut f, trigger);
    assert!(zoom_locked(&mut f, &output1));

    f.niri().remove_output(&output1);
    assert!(f.niri().zoom_lock_hold.is_none());
    assert!(!zoom_locked(&mut f, &output2));
}

// --- animated transitions ---

fn set_up_animated() -> Fixture {
    set_up_with_config(ANIMATED_CONFIG)
}

fn zoom_is_animating(f: &mut Fixture, output: &Output) -> bool {
    f.niri()
        .layout
        .monitor_for_output(output)
        .unwrap()
        .zoom()
        .is_animating()
}

fn zoom_transform(f: &mut Fixture, output: &Output) -> crate::utils::view::ViewportTransform {
    f.niri()
        .layout
        .monitor_for_output(output)
        .unwrap()
        .zoom()
        .viewport_transform()
}

#[test]
fn zoom_anim_zoom_in_smooth() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(2.)), false);
    assert!(zoom_is_animating(&mut f, &output));
    assert_eq!(zoom_level(&mut f, &output), 1.);
    assert_eq!(zoom_target_level(&mut f, &output), 2.);

    advance_clock(&mut f, 50);
    let mid = zoom_level(&mut f, &output);
    assert!(mid > 1. && mid < 2., "intermediate level: {mid}");

    advance_clock(&mut f, 5000);
    assert!(!zoom_is_animating(&mut f, &output));
    assert_eq!(zoom_level(&mut f, &output), 2.);
    assert_eq!(zoom_target_level(&mut f, &output), 2.);
}

#[test]
fn zoom_anim_zoom_out_exact_identity() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(2.)), false);
    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 2.);

    f.niri_state().do_action(Action::ResetZoom, false);
    advance_clock(&mut f, 50);
    let mid = zoom_level(&mut f, &output);
    assert!(mid > 1. && mid < 2., "intermediate level: {mid}");

    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 1.);

    // The transform is exactly the identity.
    let t = zoom_transform(&mut f, &output);
    assert_eq!(t.factor(), 1.);
    assert_eq!(
        t.apply(Point::from((123., 456.))),
        Point::from((123., 456.))
    );
}

#[test]
fn zoom_anim_zoom_out_to_one_keeps_focal_stable() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    set_zoom(&mut f, &output, 2., Point::from((500., 300.)));

    // Zooming out to the identity transform must not send the focal point
    // into a clamped corner: the anchored solve degenerates as the level
    // approaches 1, so the transition keeps the focal point fixed.
    f.niri_state().do_action(Action::ResetZoom, false);
    for _ in 0..10 {
        advance_clock(&mut f, 20);
        assert_eq!(zoom_focal(&mut f, &output), Point::from((500., 300.)));
    }
    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 1.);
    assert_eq!(zoom_focal(&mut f, &output), Point::from((500., 300.)));
}

#[test]
fn zoom_anim_toggle_zoom_hold_round_trip_keeps_focal_stable() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));
    freeze_clock(&mut f);

    // First press: locked zoom-in around the viewport center.
    f.niri_state()
        .do_action(Action::ToggleZoom(ZoomLevelPreset(2.), true), false);
    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 2.);
    assert!(zoom_locked(&mut f, &output));
    let focal = zoom_focal(&mut f, &output);
    assert_abs_diff_eq!(focal.x, 960., epsilon = EPS);
    assert_abs_diff_eq!(focal.y, 360., epsilon = EPS);

    // Second press: the zoom-out must stay locked (centered) and the focal
    // point must not fly to a clamped corner.
    f.niri_state()
        .do_action(Action::ToggleZoom(ZoomLevelPreset(2.), true), false);
    for _ in 0..10 {
        advance_clock(&mut f, 20);
        let focal = zoom_focal(&mut f, &output);
        assert_abs_diff_eq!(focal.x, 960., epsilon = EPS);
        assert_abs_diff_eq!(focal.y, 360., epsilon = EPS);
    }
    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 1.);
    assert!(!zoom_locked(&mut f, &output));
}

#[test]
fn zoom_anim_global_off_is_immediate() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));

    f.niri_state().do_action(Action::ZoomIn, false);
    assert!(!zoom_is_animating(&mut f, &output));
    assert_abs_diff_eq!(zoom_level(&mut f, &output), 1.2, epsilon = EPS);
}

#[test]
fn zoom_anim_per_zoom_off_is_immediate() {
    let mut f = set_up_with_config(&format!(
        "{ANIMATED_CONFIG}\nanimations {{\n    zoom {{\n        off\n    }}\n}}"
    ));
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(2.)), false);
    assert!(!zoom_is_animating(&mut f, &output));
    assert_eq!(zoom_level(&mut f, &output), 2.);
}

#[test]
fn zoom_anim_retarget_same_direction_no_jump() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(2.)), false);
    advance_clock(&mut f, 50);
    let before = zoom_level(&mut f, &output);
    assert!(before > 1. && before < 2.);

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(3.)), false);
    assert_eq!(zoom_target_level(&mut f, &output), 3.);
    assert_abs_diff_eq!(zoom_level(&mut f, &output), before, epsilon = EPS);

    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 3.);
}

#[test]
fn zoom_anim_retarget_reverse_no_jump() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(3.)), false);
    advance_clock(&mut f, 50);
    let before = zoom_level(&mut f, &output);
    assert!(before > 1. && before < 3.);

    f.niri_state().do_action(Action::ResetZoom, false);
    assert_eq!(zoom_target_level(&mut f, &output), 1.);
    assert_abs_diff_eq!(zoom_level(&mut f, &output), before, epsilon = EPS);

    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 1.);
}

#[test]
fn zoom_anim_repeated_zoom_in_targets() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    // Repeated presses stack on the target level even while the displayed
    // level is still catching up.
    for expected in [1.2, 1.44, 1.728] {
        f.niri_state().do_action(Action::ZoomIn, false);
        assert_abs_diff_eq!(zoom_target_level(&mut f, &output), expected, epsilon = EPS);
    }
    assert!(zoom_level(&mut f, &output) < 1.44);

    advance_clock(&mut f, 5000);
    assert_abs_diff_eq!(zoom_level(&mut f, &output), 1.728, epsilon = EPS);
}

#[test]
fn zoom_anim_toggle_mid_animation() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    f.niri_state()
        .do_action(Action::ToggleZoom(ZoomLevelPreset(2.), false), false);
    advance_clock(&mut f, 50);
    assert!(zoom_level(&mut f, &output) > 1.);

    // Toggling mid-flight targets 1 regardless of the displayed level.
    f.niri_state()
        .do_action(Action::ToggleZoom(ZoomLevelPreset(2.), false), false);
    assert_eq!(zoom_target_level(&mut f, &output), 1.);

    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 1.);
}

#[test]
fn zoom_anim_relative_pointer_scaled_by_current_level() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    let id = f.add_client();
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(2.)), false);
    advance_clock(&mut f, 50);
    let level = zoom_level(&mut f, &output);
    assert!(level > 1. && level < 2.);

    let before = pointer_location(&mut f);
    move_pointer(&mut f, id, 10., 0.);
    let after = pointer_location(&mut f);

    // The relative delta is divided by the currently displayed level.
    assert_abs_diff_eq!(after.x - before.x, 10. / level, epsilon = 1e-6);
}

#[test]
fn zoom_anim_absolute_pointer_inverse_uses_current_transform() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    let id = f.add_client();
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(2.)), false);
    advance_clock(&mut f, 50);
    let transform = zoom_transform(&mut f, &output);
    assert!(transform.factor() > 1.);

    let bounds = global_output_bounds(&mut f);
    let extent = bounds.size;
    let display = Point::from((700., 300.));
    move_pointer_absolute(&mut f, id, display, extent, None);

    // The canonical position is the inverse of the animated transform.
    let expected = transform.apply_inverse(display);
    let actual = pointer_location(&mut f);
    assert_abs_diff_eq!(actual.x, expected.x, epsilon = 1e-6);
    assert_abs_diff_eq!(actual.y, expected.y, epsilon = 1e-6);
}

#[test]
fn zoom_anim_locked_viewport_clamps_pointer() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(2.)), false);
    f.niri()
        .layout
        .monitor_for_output_mut(&output)
        .unwrap()
        .zoom_mut()
        .set_locked(true);
    advance_clock(&mut f, 50);

    let viewport = f
        .niri()
        .layout
        .monitor_for_output(&output)
        .unwrap()
        .zoom()
        .viewport();

    // A warp outside the animated viewport clamps to its current bounds.
    f.niri_state().move_cursor(Point::from((10., 360.)));
    let pos = pointer_location(&mut f);
    assert_abs_diff_eq!(pos.x, viewport.loc.x, epsilon = 1e-6);
}

#[test]
fn zoom_anim_deadzone_drifts_during_animation() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(2.)), false);
    advance_clock(&mut f, 50);
    let level = zoom_level(&mut f, &output);
    let focal_before = zoom_focal(&mut f, &output);

    // A warp to the content corner leaves the deadzone: the level animation
    // keeps owning the viewport, but the anchor's display position starts
    // drifting towards the deadzone edge immediately — the camera moves
    // during the zoom animation, not after it.
    f.niri_state().move_cursor(Point::from((0., 0.)));
    let focal_after_warp = zoom_focal(&mut f, &output);
    assert_abs_diff_eq!(focal_after_warp.x, focal_before.x, epsilon = 1e-6);
    assert_abs_diff_eq!(focal_after_warp.y, focal_before.y, epsilon = 1e-6);
    assert!(zoom_is_animating(&mut f, &output));
    assert_eq!(zoom_level(&mut f, &output), level);

    advance_clock(&mut f, 50);
    assert_ne!(zoom_focal(&mut f, &output), focal_after_warp);

    // The drift completes together with the level animation: the camera is
    // already at the deadzone edge and no separate follow phase runs.
    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 2.);
    assert_eq!(zoom_focal(&mut f, &output), Point::from((0., 0.)));
    assert!(!zoom_is_animating(&mut f, &output));
}

#[test]
fn zoom_anim_locked_keeps_viewport_center_fixed() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    set_zoom(&mut f, &output, 2., Point::from((500., 300.)));
    let center_before = crate::utils::center_f64(
        f.niri()
            .layout
            .monitor_for_output(&output)
            .unwrap()
            .zoom()
            .viewport(),
    );
    f.niri()
        .layout
        .monitor_for_output_mut(&output)
        .unwrap()
        .zoom_mut()
        .set_locked(true);

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(4.)), false);
    for _ in 0..10 {
        advance_clock(&mut f, 20);
        let center = crate::utils::center_f64(
            f.niri()
                .layout
                .monitor_for_output(&output)
                .unwrap()
                .zoom()
                .viewport(),
        );
        assert_abs_diff_eq!(center.x, center_before.x, epsilon = EPS);
        assert_abs_diff_eq!(center.y, center_before.y, epsilon = EPS);
    }
    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 4.);
    let center = crate::utils::center_f64(
        f.niri()
            .layout
            .monitor_for_output(&output)
            .unwrap()
            .zoom()
            .viewport(),
    );
    assert_abs_diff_eq!(center.x, center_before.x, epsilon = EPS);
    assert_abs_diff_eq!(center.y, center_before.y, epsilon = EPS);
}

#[test]
fn zoom_anim_lock_mid_animation_freezes_focal() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(3.)), false);
    advance_clock(&mut f, 50);

    let frozen = zoom_focal(&mut f, &output);
    f.niri_state().do_action(Action::ZoomLock(false), false);

    for _ in 0..10 {
        advance_clock(&mut f, 20);
        assert_eq!(zoom_focal(&mut f, &output), frozen);
    }
    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 3.);
}

#[test]
fn zoom_anim_unlock_mid_animation_no_jump() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(3.)), false);
    advance_clock(&mut f, 50);

    f.niri_state().do_action(Action::ZoomLock(false), false);
    let frozen = zoom_focal(&mut f, &output);
    advance_clock(&mut f, 20);

    f.niri_state().do_action(Action::ZoomLock(false), false);
    assert_eq!(zoom_focal(&mut f, &output), frozen);

    // Deadzone tracking waits for the level animation to finish.
    f.niri_state().move_cursor(Point::from((0., 0.)));
    assert_eq!(zoom_focal(&mut f, &output), frozen);

    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 3.);

    advance_clock(&mut f, 5000);
    assert_eq!(zoom_focal(&mut f, &output), Point::from((0., 0.)));
}

#[test]
fn zoom_anim_multi_output_isolated() {
    let mut f = set_up_animated();
    let output1 = f.niri_output(1);
    f.add_output(2, (1920, 720));
    let output2 = f.niri_output(2);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(2.)), false);
    advance_clock(&mut f, 50);

    assert!(zoom_is_animating(&mut f, &output1));
    assert!(!zoom_is_animating(&mut f, &output2));
    assert_eq!(zoom_level(&mut f, &output2), 1.);

    // Redraw scheduling is per-output.
    assert!(f.niri().layout.are_animations_ongoing(Some(&output1)));
    assert!(!f.niri().layout.are_animations_ongoing(Some(&output2)));

    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output1), 2.);
    assert_eq!(zoom_level(&mut f, &output2), 1.);
}

#[test]
fn zoom_anim_max_reload_hard_clamp_cancels() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(8.)), false);
    advance_clock(&mut f, 50);
    let mid = zoom_level(&mut f, &output);
    assert!(mid > 1. && mid < 8.);

    // Lowering the maximum below the displayed level clamps immediately.
    reload_with_animated(&mut f, "zoom { max-zoom 2; }");
    assert!(!zoom_is_animating(&mut f, &output));
    assert_eq!(zoom_level(&mut f, &output), 2.);
    assert_eq!(zoom_target_level(&mut f, &output), 2.);
}

#[test]
fn zoom_anim_max_reload_retargets() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(8.)), false);
    // At t=0 the displayed level is still 1, below the new maximum.
    assert_eq!(zoom_level(&mut f, &output), 1.);

    reload_with_animated(&mut f, "zoom { max-zoom 2; }");
    assert_eq!(zoom_target_level(&mut f, &output), 2.);
    assert!(zoom_is_animating(&mut f, &output));

    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 2.);
}

#[test]
fn zoom_anim_config_reload_applies_to_next_retarget() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(2.)), false);
    advance_clock(&mut f, 50);
    assert!(zoom_is_animating(&mut f, &output));

    // Turning the zoom animation off does not affect the in-flight
    // transition, only the next retarget.
    reload_with_animated(&mut f, "animations {\n    zoom {\n        off\n    }\n}");
    assert!(zoom_is_animating(&mut f, &output));

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(3.)), false);
    assert!(!zoom_is_animating(&mut f, &output));
    assert_eq!(zoom_level(&mut f, &output), 3.);
}

#[test]
fn zoom_anim_global_off_reload_completes() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(2.)), false);
    assert!(zoom_is_animating(&mut f, &output));

    // Turning all animations off completes the transition on the next
    // advance through the shared clock.
    reload_with_animated(&mut f, "animations {\n    off\n}");
    f.niri().advance_animations();
    assert!(!zoom_is_animating(&mut f, &output));
    assert_eq!(zoom_level(&mut f, &output), 2.);
}

#[test]
fn zoom_anim_hold_during_animation_snapshots_displayed() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(2.)), false);
    advance_clock(&mut f, 50);
    let mid = zoom_level(&mut f, &output);
    assert!(mid > 1. && mid < 2.);

    // Beginning a hold snapshots the currently displayed state and animates
    // towards the preset like a regular zoom action.
    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 3.);
    assert!(zoom_is_animating(&mut f, &output));
    assert_eq!(zoom_target_level(&mut f, &output), 3.);

    let hold = f.niri().zoom_hold.as_ref().unwrap();
    assert_abs_diff_eq!(hold.previous.level, mid, epsilon = EPS);
    assert_eq!(hold.previous.target_level, 2.);

    // The release animates back to the saved target, not the mid-flight
    // displayed level.
    hold_release(&mut f, trigger);
    assert!(zoom_is_animating(&mut f, &output));
    assert_eq!(zoom_target_level(&mut f, &output), 2.);

    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 2.);
    assert_eq!(zoom_target_level(&mut f, &output), 2.);
}

#[test]
fn zoom_anim_redraw_scheduled_while_animating() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(2.)), false);
    assert!(f.niri().layout.are_animations_ongoing(Some(&output)));

    advance_clock(&mut f, 5000);
    assert!(!f.niri().layout.are_animations_ongoing(Some(&output)));
}

// --- animated hold-zoom lifecycle ---

/// Resizes the output's mode and notifies the compositor.
fn resize_output(f: &mut Fixture, output: &Output, size: (i32, i32)) {
    output.change_current_state(
        Some(Mode {
            size: Size::from(size),
            refresh: 60_000,
        }),
        None,
        None,
        None,
    );
    f.niri().output_resized(output);
}

#[test]
fn zoom_anim_hold_begin_smooth() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 2.);
    assert!(zoom_is_animating(&mut f, &output));
    assert_eq!(zoom_level(&mut f, &output), 1.);
    assert_eq!(zoom_target_level(&mut f, &output), 2.);
    assert!(f.niri().zoom_hold.is_some());

    advance_clock(&mut f, 50);
    let mid = zoom_level(&mut f, &output);
    assert!(mid > 1. && mid < 2., "intermediate level: {mid}");

    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 2.);
}

#[test]
fn zoom_anim_hold_release_smooth() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 2.);
    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 2.);

    hold_release(&mut f, trigger);
    assert!(f.niri().zoom_hold.is_none());
    assert!(zoom_is_animating(&mut f, &output));
    assert_eq!(zoom_target_level(&mut f, &output), 1.);

    advance_clock(&mut f, 50);
    let mid = zoom_level(&mut f, &output);
    assert!(mid > 1. && mid < 2., "intermediate level: {mid}");

    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 1.);
    assert_eq!(zoom_target_level(&mut f, &output), 1.);
    assert!(!zoom_is_animating(&mut f, &output));
}

#[test]
fn zoom_anim_hold_release_restores_non_one() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    set_zoom(&mut f, &output, 1.5, Point::from((960., 360.)));

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 3.);
    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 3.);

    hold_release(&mut f, trigger);
    assert!(zoom_is_animating(&mut f, &output));

    advance_clock(&mut f, 50);
    let mid = zoom_level(&mut f, &output);
    assert!(mid > 1.5 && mid < 3., "intermediate level: {mid}");

    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 1.5);
    assert_eq!(zoom_target_level(&mut f, &output), 1.5);
}

#[test]
fn zoom_anim_hold_release_restores_focal() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    let saved_focal = zoom_focal(&mut f, &output);

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 3.);
    advance_clock(&mut f, 5000);
    // Move the camera during the hold: a cursor warp to the content corner
    // pushes the focal point to the output edge once the camera settles.
    f.niri_state().move_cursor(Point::from((0., 0.)));
    advance_clock(&mut f, 5000);
    let held_focal = zoom_focal(&mut f, &output);
    assert_ne!(held_focal, saved_focal);

    // Bring the pointer back inside the deadzone so the restore is not
    // immediately retargeted by the follow evaluation.
    f.niri_state().move_cursor(Point::from((960., 360.)));

    hold_release(&mut f, trigger);
    assert!(zoom_is_animating(&mut f, &output));

    advance_clock(&mut f, 50);
    let mid_focal = zoom_focal(&mut f, &output);
    assert!(
        mid_focal.x > 0. && mid_focal.x < saved_focal.x,
        "intermediate focal: {mid_focal:?} between {held_focal:?} and {saved_focal:?}"
    );

    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 2.);
    assert_eq!(zoom_focal(&mut f, &output), saved_focal);
}

#[test]
fn zoom_anim_hold_same_level_focal_restore() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    let saved_focal = zoom_focal(&mut f, &output);

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 3.);
    advance_clock(&mut f, 5000);
    // Return to the saved level but with a different focal point: the release
    // must animate the focal point without moving the level.
    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(2.)), false);
    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 2.);
    f.niri_state().move_cursor(Point::from((0., 0.)));
    advance_clock(&mut f, 5000);
    assert_ne!(zoom_focal(&mut f, &output), saved_focal);

    // Bring the pointer back inside the deadzone so the restore is not
    // immediately retargeted by the follow evaluation.
    f.niri_state().move_cursor(Point::from((960., 360.)));

    hold_release(&mut f, trigger);
    assert!(zoom_is_animating(&mut f, &output));

    for _ in 0..10 {
        advance_clock(&mut f, 20);
        assert_eq!(zoom_level(&mut f, &output), 2., "level must not move");
    }

    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 2.);
    assert_eq!(zoom_focal(&mut f, &output), saved_focal);
}

#[test]
fn zoom_anim_hold_actions_during_hold_then_restore() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    set_zoom(&mut f, &output, 1.5, Point::from((960., 360.)));
    let saved_focal = zoom_focal(&mut f, &output);

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 2.);
    advance_clock(&mut f, 5000);

    // Regular zoom actions keep working during the hold.
    f.niri_state().do_action(Action::ZoomIn, false);
    f.niri_state().do_action(Action::ZoomIn, false);
    advance_clock(&mut f, 5000);
    assert_abs_diff_eq!(zoom_level(&mut f, &output), 2.88, epsilon = EPS);

    // The release still restores the pre-hold state.
    hold_release(&mut f, trigger);
    assert!(zoom_is_animating(&mut f, &output));
    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 1.5);
    assert_eq!(zoom_target_level(&mut f, &output), 1.5);
    assert_eq!(zoom_focal(&mut f, &output), saved_focal);
}

#[test]
fn zoom_anim_hold_reset_and_toggle_during_hold() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    set_zoom(&mut f, &output, 1.5, Point::from((960., 360.)));

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 3.);
    advance_clock(&mut f, 5000);

    f.niri_state().do_action(Action::ResetZoom, false);
    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 1.);
    assert!(f.niri().zoom_hold.is_some());

    f.niri_state()
        .do_action(Action::ToggleZoom(ZoomLevelPreset(2.), false), false);
    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 2.);

    hold_release(&mut f, trigger);
    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 1.5);
    assert_eq!(zoom_target_level(&mut f, &output), 1.5);
}

#[test]
fn zoom_anim_hold_deadzone_during_restore() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    let id = f.add_client();
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 3.);
    advance_clock(&mut f, 5000);

    hold_release(&mut f, trigger);
    advance_clock(&mut f, 50);
    assert!(zoom_is_animating(&mut f, &output));

    // Pointer tracking during the restore takes over the camera: the restore
    // converts to a regular level animation anchored on the cursor, so the
    // displayed focal stays continuous while the level keeps animating to
    // the saved target. The relative delta is scaled by the displayed level,
    // so use a large one.
    let focal_before = zoom_focal(&mut f, &output);
    move_pointer(&mut f, id, -5000., -5000.);
    assert_abs_diff_eq!(zoom_focal(&mut f, &output).x, focal_before.x, epsilon = EPS);
    assert_abs_diff_eq!(zoom_focal(&mut f, &output).y, focal_before.y, epsilon = EPS);
    assert!(zoom_is_animating(&mut f, &output));

    // Once the level animation completes, the camera follows the pointer to
    // the deadzone edge.
    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 2.);
    advance_clock(&mut f, 5000);
    assert_eq!(zoom_focal(&mut f, &output), Point::from((0., 0.)));
}

#[test]
fn zoom_anim_hold_warp_during_restore() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    let saved_focal = zoom_focal(&mut f, &output);

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 3.);
    advance_clock(&mut f, 5000);

    hold_release(&mut f, trigger);
    advance_clock(&mut f, 50);
    // A programmatic warp teleports the cursor; the restore converts to a
    // regular level animation anchored on the cursor, so the displayed focal
    let focal_before = zoom_focal(&mut f, &output);
    f.niri_state().move_cursor(Point::from((0., 0.)));
    assert_abs_diff_eq!(zoom_focal(&mut f, &output).x, focal_before.x, epsilon = EPS);
    assert_abs_diff_eq!(zoom_focal(&mut f, &output).y, focal_before.y, epsilon = EPS);

    assert!(zoom_is_animating(&mut f, &output));

    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 2.);
    advance_clock(&mut f, 5000);
    assert_eq!(zoom_focal(&mut f, &output), Point::from((0., 0.)));
    assert_ne!(zoom_focal(&mut f, &output), saved_focal);
}

#[test]
fn zoom_anim_hold_zoom_action_during_restore() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    set_zoom(&mut f, &output, 1.5, Point::from((960., 360.)));

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 3.);
    advance_clock(&mut f, 5000);

    hold_release(&mut f, trigger);
    advance_clock(&mut f, 50);
    let mid = zoom_level(&mut f, &output);
    assert!(mid > 1.5 && mid < 3.);

    // A new zoom action retargets from the displayed state without a jump.
    f.niri_state().do_action(Action::ZoomIn, false);
    assert_abs_diff_eq!(zoom_level(&mut f, &output), mid, epsilon = EPS);
    assert!(zoom_is_animating(&mut f, &output));

    advance_clock(&mut f, 5000);
    assert_abs_diff_eq!(zoom_level(&mut f, &output), 1.8, epsilon = EPS);
}

#[test]
fn zoom_anim_hold_toggle_during_restore() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    set_zoom(&mut f, &output, 1.5, Point::from((960., 360.)));

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 3.);
    advance_clock(&mut f, 5000);

    hold_release(&mut f, trigger);
    advance_clock(&mut f, 50);
    let mid = zoom_level(&mut f, &output);

    // The toggle decision uses the restore's target (1.5 > 1), so it resets
    // to 1 from the displayed state without a jump.
    f.niri_state()
        .do_action(Action::ToggleZoom(ZoomLevelPreset(4.), false), false);
    assert_eq!(zoom_target_level(&mut f, &output), 1.);
    assert_abs_diff_eq!(zoom_level(&mut f, &output), mid, epsilon = EPS);

    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 1.);
}

#[test]
fn zoom_anim_hold_lock_during_restore() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    let saved_focal = zoom_focal(&mut f, &output);

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 3.);
    advance_clock(&mut f, 5000);

    // Move the camera during the hold so the restore has a focal destination
    // different from the current one.
    f.niri_state().move_cursor(Point::from((0., 0.)));
    advance_clock(&mut f, 5000);
    assert_ne!(zoom_focal(&mut f, &output), saved_focal);

    hold_release(&mut f, trigger);
    advance_clock(&mut f, 50);
    assert!(zoom_is_animating(&mut f, &output));

    // Locking mid-restore freezes the current focal point; the saved focal
    // destination is discarded while the level keeps animating.
    let frozen = zoom_focal(&mut f, &output);
    f.niri_state().do_action(Action::ZoomLock(false), false);
    assert!(zoom_locked(&mut f, &output));

    for _ in 0..10 {
        advance_clock(&mut f, 20);
        assert_eq!(zoom_focal(&mut f, &output), frozen);
    }

    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 2.);
    assert_eq!(zoom_focal(&mut f, &output), frozen);
    assert_ne!(frozen, saved_focal);
}

#[test]
fn zoom_anim_hold_locked_at_release() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    let saved_focal = zoom_focal(&mut f, &output);

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 3.);
    advance_clock(&mut f, 5000);

    // Move the camera during the hold, then lock it: the release must not
    // restore the saved focal point.
    f.niri_state().move_cursor(Point::from((0., 0.)));
    advance_clock(&mut f, 5000);
    f.niri_state().do_action(Action::ZoomLock(false), false);
    let locked_focal = zoom_focal(&mut f, &output);

    hold_release(&mut f, trigger);
    assert!(zoom_is_animating(&mut f, &output));

    for _ in 0..10 {
        advance_clock(&mut f, 20);
        assert_eq!(zoom_focal(&mut f, &output), locked_focal);
    }

    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 2.);
    assert_eq!(zoom_focal(&mut f, &output), locked_focal);
    assert_ne!(locked_focal, saved_focal);
    assert!(zoom_locked(&mut f, &output));
}

#[test]
fn zoom_anim_hold_reentry_same_output() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    set_zoom(&mut f, &output, 1.5, Point::from((960., 360.)));
    let saved_focal = zoom_focal(&mut f, &output);

    let trigger_a = key_trigger(30);
    let trigger_b = key_trigger(48);

    hold_press(&mut f, trigger_a, 2.);
    advance_clock(&mut f, 50);
    assert!(zoom_is_animating(&mut f, &output));

    // A second hold on the same output replaces the session without an
    // intermediate restore: the original pre-hold snapshot is kept.
    hold_press(&mut f, trigger_b, 4.);
    assert_eq!(zoom_target_level(&mut f, &output), 4.);
    let hold = f.niri().zoom_hold.as_ref().unwrap();
    assert_eq!(hold.trigger, trigger_b);
    assert_eq!(hold.previous.target_level, 1.5);
    assert_eq!(hold.previous.focal, saved_focal);

    // Releasing the replaced trigger is a no-op.
    hold_release(&mut f, trigger_a);
    assert!(f.niri().zoom_hold.is_some());
    assert_eq!(zoom_target_level(&mut f, &output), 4.);

    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 4.);

    // Releasing the owner restores the original pre-hold state.
    hold_release(&mut f, trigger_b);
    assert!(zoom_is_animating(&mut f, &output));
    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 1.5);
    assert_eq!(zoom_focal(&mut f, &output), saved_focal);
}

#[test]
fn zoom_anim_hold_reentry_cross_output() {
    let mut f = set_up_animated();
    let output1 = f.niri_output(1);
    f.add_output(2, (1920, 720));
    let output2 = f.niri_output(2);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    set_zoom(&mut f, &output1, 1.5, Point::from((960., 360.)));

    let trigger_a = key_trigger(30);
    let trigger_b = key_trigger(48);

    hold_press(&mut f, trigger_a, 2.);
    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output1), 2.);

    // A hold press on another output ends the first session with an animated
    // restore and starts a new session on the pointer output.
    let geo2 = f.niri().global_space.output_geometry(&output2).unwrap();
    f.niri_state()
        .move_cursor(geo2.loc.to_f64() + Point::from((150., 100.)));
    hold_press(&mut f, trigger_b, 3.);

    assert!(zoom_is_animating(&mut f, &output1));
    assert!(zoom_is_animating(&mut f, &output2));
    assert_eq!(zoom_target_level(&mut f, &output1), 1.5);
    assert_eq!(zoom_target_level(&mut f, &output2), 3.);

    // Releasing the old trigger is a no-op; the restore on output 1 is a
    // regular transition, not a hold session.
    hold_release(&mut f, trigger_a);
    assert!(f.niri().zoom_hold.is_some());

    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output1), 1.5);
    assert_eq!(zoom_level(&mut f, &output2), 3.);

    hold_release(&mut f, trigger_b);
    assert!(zoom_is_animating(&mut f, &output2));
    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output2), 1.);
}

#[test]
fn zoom_anim_hold_reload_lower_max() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    // Pre-hold state above the future maximum.
    set_zoom(&mut f, &output, 6., Point::from((960., 360.)));

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 2.);
    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 2.);

    // Lowering max-zoom during the hold clamps the restore destination too.
    reload_with_animated(&mut f, "zoom { max-zoom 3; }");

    hold_release(&mut f, trigger);
    assert_eq!(zoom_target_level(&mut f, &output), 3.);
    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 3.);
    assert_eq!(zoom_target_level(&mut f, &output), 3.);
}

#[test]
fn zoom_anim_hold_resize_during_hold() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    // A focal point valid for 1920x720 but out of bounds for 960x540.
    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    f.niri_state().move_cursor(Point::from((1900., 700.)));
    advance_clock(&mut f, 5000);
    let saved_focal = zoom_focal(&mut f, &output);
    assert_eq!(saved_focal, Point::from((1920., 720.)));

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 3.);
    advance_clock(&mut f, 5000);

    // Shrinking the output during the hold clamps the restore destination to
    // the current geometry.
    resize_output(&mut f, &output, (960, 540));

    hold_release(&mut f, trigger);
    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 2.);
    let focal = zoom_focal(&mut f, &output);
    assert!(focal.x <= 960. && focal.y <= 540.);
    assert_eq!(focal, Point::from((960., 540.)));
}

#[test]
fn zoom_anim_hold_resize_during_restore() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    f.niri_state().move_cursor(Point::from((1900., 700.)));

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 3.);
    advance_clock(&mut f, 5000);

    hold_release(&mut f, trigger);
    advance_clock(&mut f, 50);
    assert!(zoom_is_animating(&mut f, &output));

    // Resizing mid-restore must not panic or produce a non-finite focal.
    resize_output(&mut f, &output, (960, 540));
    let focal = zoom_focal(&mut f, &output);
    assert!(focal.x.is_finite() && focal.y.is_finite());

    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 2.);
    let focal = zoom_focal(&mut f, &output);
    assert!(focal.x <= 960. && focal.y <= 540.);
}

#[test]
fn zoom_anim_hold_per_zoom_off_is_immediate() {
    let mut f = set_up_with_config(&format!(
        "{ANIMATED_CONFIG}\nanimations {{\n    zoom {{\n        off\n    }}\n}}"
    ));
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));

    set_zoom(&mut f, &output, 1.5, Point::from((960., 360.)));

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 3.);
    assert!(!zoom_is_animating(&mut f, &output));
    assert_eq!(zoom_level(&mut f, &output), 3.);

    hold_release(&mut f, trigger);
    assert!(!zoom_is_animating(&mut f, &output));
    assert_eq!(zoom_level(&mut f, &output), 1.5);
}

#[test]
fn zoom_anim_hold_suspend_restores_immediately() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    set_zoom(&mut f, &output, 1.5, Point::from((960., 360.)));

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 2.);
    advance_clock(&mut f, 5000);

    // Suspend may never deliver the release; the restore is immediate.
    f.niri_state().do_action(Action::Suspend, false);
    assert!(!zoom_is_animating(&mut f, &output));
    assert_eq!(zoom_level(&mut f, &output), 1.5);
    assert!(f.niri().zoom_hold.is_none());
}

#[test]
fn zoom_anim_hold_global_off_is_immediate() {
    let mut f = set_up();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));

    set_zoom(&mut f, &output, 1.5, Point::from((960., 360.)));

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 3.);
    assert!(!zoom_is_animating(&mut f, &output));
    assert_eq!(zoom_level(&mut f, &output), 3.);

    hold_release(&mut f, trigger);
    assert!(!zoom_is_animating(&mut f, &output));
    assert_eq!(zoom_level(&mut f, &output), 1.5);
}

#[test]
fn zoom_anim_hold_release_vs_cleanup() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    // A user release animates the restore.
    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 2.);
    advance_clock(&mut f, 5000);
    hold_release(&mut f, trigger);
    assert!(zoom_is_animating(&mut f, &output));
    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 1.);

    // A lost-release cleanup restores immediately.
    hold_press(&mut f, trigger, 2.);
    advance_clock(&mut f, 5000);
    f.niri_state().cancel_zoom_hold_immediate();
    assert!(!zoom_is_animating(&mut f, &output));
    assert_eq!(zoom_level(&mut f, &output), 1.);
    assert!(f.niri().zoom_hold.is_none());
}

#[test]
fn zoom_anim_hold_vt_switch_restores_immediately() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    set_zoom(&mut f, &output, 1.5, Point::from((960., 360.)));

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 2.);
    advance_clock(&mut f, 5000);

    // A VT switch may never deliver the release; the restore is immediate.
    f.niri_state().do_action(Action::ChangeVt(2), false);
    assert!(!zoom_is_animating(&mut f, &output));
    assert_eq!(zoom_level(&mut f, &output), 1.5);
    assert!(f.niri().zoom_hold.is_none());
}

#[test]
fn zoom_anim_hold_screenshot_ui_release() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    set_zoom(&mut f, &output, 1.5, Point::from((960., 360.)));

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 2.);
    advance_clock(&mut f, 5000);

    // The release is a cleanup path, not a new action: the screenshot UI
    // allow-list must not block the animated restore.
    f.niri_state().open_screenshot_ui(true, None);
    assert!(f.niri().screenshot_ui.is_open());

    hold_release(&mut f, trigger);
    assert!(f.niri().zoom_hold.is_none());
    assert!(zoom_is_animating(&mut f, &output));

    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 1.5);
}

#[test]
fn zoom_anim_hold_multi_output_independence() {
    let mut f = set_up_animated();
    let output1 = f.niri_output(1);
    f.add_output(2, (1920, 720));
    let output2 = f.niri_output(2);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    set_zoom(&mut f, &output2, 4., Point::from((960., 360.)));

    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 2.);
    advance_clock(&mut f, 5000);

    hold_release(&mut f, trigger);
    assert!(zoom_is_animating(&mut f, &output1));
    assert!(!zoom_is_animating(&mut f, &output2));

    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output1), 1.);
    // The restore on output 1 does not touch output 2.
    assert_eq!(zoom_level(&mut f, &output2), 4.);
}

fn effective_zoom_transform(
    f: &mut Fixture,
    output: &Output,
) -> crate::utils::view::ViewportTransform {
    f.niri()
        .layout
        .monitor_for_output(output)
        .unwrap()
        .effective_zoom_transform()
}

fn set_overview_progress(f: &mut Fixture, _output: &Output, progress: Option<f64>) {
    f.niri().layout.set_overview_progress_for_test(progress);
}

fn set_overview_open(f: &mut Fixture, _output: &Output, open: bool) {
    f.niri().layout.set_overview_open_for_test(open);
}

#[test]
fn zoom_overview_effective_transform_math() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let focal = Point::from((100., 200.));

    set_zoom(&mut f, &output, 4., focal);
    let stored = zoom_transform(&mut f, &output);
    assert_eq!(effective_zoom_transform(&mut f, &output), stored);

    set_overview_progress(&mut f, &output, Some(0.));
    assert_eq!(effective_zoom_transform(&mut f, &output), stored);

    set_overview_progress(&mut f, &output, Some(0.5));
    let partial = effective_zoom_transform(&mut f, &output);
    assert_abs_diff_eq!(partial.factor(), 2., epsilon = EPS);
    assert_eq!(partial.focal(), focal);

    let view_size = f
        .niri()
        .layout
        .monitor_for_output(&output)
        .unwrap()
        .view_size();
    let expected_viewport = crate::utils::view::ViewportTransform::new(focal, 2.)
        .apply_inverse_rect(Rectangle::from_size(view_size));
    let viewport = f
        .niri()
        .layout
        .monitor_for_output(&output)
        .unwrap()
        .effective_viewport();
    assert_eq!(viewport, expected_viewport);

    set_overview_progress(&mut f, &output, Some(1.));
    assert_eq!(
        effective_zoom_transform(&mut f, &output),
        crate::utils::view::ViewportTransform::identity()
    );

    set_overview_progress(&mut f, &output, Some(-0.5));
    assert_eq!(effective_zoom_transform(&mut f, &output), stored);
    set_overview_progress(&mut f, &output, Some(1.5));
    assert_eq!(
        effective_zoom_transform(&mut f, &output),
        crate::utils::view::ViewportTransform::identity()
    );
}

#[test]
fn zoom_overview_effective_transform_reverses_continuously() {
    let mut f = set_up();
    let output = f.niri_output(1);
    set_zoom(&mut f, &output, 4., Point::from((960., 360.)));

    let opening = [0., 0.25, 0.5, 0.75, 1.]
        .into_iter()
        .map(|progress| {
            set_overview_progress(&mut f, &output, Some(progress));
            effective_zoom_transform(&mut f, &output).factor()
        })
        .collect::<Vec<_>>();
    assert!(opening.windows(2).all(|pair| pair[0] >= pair[1]));
    assert_eq!(opening[0], 4.);
    assert_eq!(opening[4], 1.);

    set_overview_progress(&mut f, &output, Some(0.4));
    let before_reversal = effective_zoom_transform(&mut f, &output).factor();
    set_overview_progress(&mut f, &output, Some(0.));
    assert!(effective_zoom_transform(&mut f, &output).factor() > before_reversal);

    set_overview_progress(&mut f, &output, Some(0.6));
    let before_close_reversal = effective_zoom_transform(&mut f, &output).factor();
    set_overview_progress(&mut f, &output, Some(1.));
    assert!(effective_zoom_transform(&mut f, &output).factor() < before_close_reversal);
}

#[test]
fn zoom_overview_render_targets_use_effective_identity() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let id = f.add_client();
    open_window(&mut f, id, "overview-render", 400, 300, [0xff, 0, 0, 0xff]);
    add_top_layer(&mut f, id, 50);
    set_zoom(&mut f, &output, 4., Point::from((100., 100.)));

    set_overview_open(&mut f, &output, true);
    for target in [
        RenderTarget::Output,
        RenderTarget::Screencast,
        RenderTarget::ScreenCapture,
    ] {
        let elements = render_elements(f.niri_state(), &output, target);
        assert!(
            elements.iter().all(|element| !is_zoomed(element)),
            "fully-open Overview must use identity presentation for {target:?}"
        );
    }

    set_overview_progress(&mut f, &output, Some(0.5));
    let elements = render_elements(f.niri_state(), &output, RenderTarget::Output);
    assert!(
        elements.iter().any(is_zoomed),
        "partial Overview must retain the unsuppressed part of desktop zoom"
    );
}

#[test]
fn zoom_overview_pointer_input_and_lock_use_effective_viewport() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let id = f.add_client();
    let center = Point::from((960., 360.));
    let extent = f.niri().global_space.output_geometry(&output).unwrap().size;

    set_zoom(&mut f, &output, 4., center);
    f.niri()
        .layout
        .monitor_for_output_mut(&output)
        .unwrap()
        .zoom_mut()
        .set_locked(true);
    f.niri_state().move_cursor(center);

    set_overview_open(&mut f, &output, true);
    move_pointer(&mut f, id, 20., 10.);
    assert_eq!(pointer_location(&mut f), center + Point::from((20., 10.)));

    move_pointer_absolute(&mut f, id, Point::from((1200., 360.)), extent, None);
    assert_eq!(pointer_location(&mut f), Point::from((1200., 360.)));

    set_overview_progress(&mut f, &output, Some(0.5));
    f.niri_state().move_cursor(center);
    move_pointer(&mut f, id, 20., 10.);
    assert_eq!(pointer_location(&mut f), center + Point::from((10., 5.)));

    move_pointer_absolute(&mut f, id, Point::from((1400., 360.)), extent, None);
    assert_eq!(pointer_location(&mut f), Point::from((1180., 360.)));

    f.niri_state().move_cursor(center);
    move_pointer(&mut f, id, 2000., 0.);
    assert_eq!(pointer_location(&mut f), Point::from((1440., 360.)));

    set_overview_open(&mut f, &output, true);
    f.niri_state().move_cursor(center);
    move_pointer(&mut f, id, 2000., 0.);
    assert_eq!(pointer_location(&mut f), Point::from((1919., 360.)));
}

#[test]
fn zoom_overview_pointer_visual_and_deadzone_are_presentation_bound() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let focal = Point::from((960., 360.));
    let content = Point::from((1060., 360.));

    set_zoom(&mut f, &output, 4., focal);
    f.niri_state().move_cursor(content);
    let before_focal = zoom_focal(&mut f, &output);

    set_overview_open(&mut f, &output, true);
    let hotspot = cursor_hotspot(&mut f, &output);
    let scale = Scale::from(output.current_scale().fractional_scale());
    let locs = pointer_element_locs(f.niri_state(), &output);
    assert_eq!(locs.len(), 1);
    assert_eq!(
        locs[0],
        (content - hotspot).to_physical_precise_round(scale)
    );
    assert_eq!(zoom_focal(&mut f, &output), before_focal);

    set_overview_progress(&mut f, &output, Some(0.5));
    let locs = pointer_element_locs(f.niri_state(), &output);
    let displayed = Point::from((
        focal.x + (content.x - focal.x) * 2.,
        focal.y + (content.y - focal.y) * 2.,
    ));
    assert_eq!(
        locs[0],
        (displayed - hotspot).to_physical_precise_round(scale)
    );

    f.niri_state().move_cursor(Point::from((0., 0.)));
    assert_eq!(zoom_focal(&mut f, &output), before_focal);
    silent_warp(&mut f, Point::from((1900., 700.)));
    assert_eq!(zoom_focal(&mut f, &output), before_focal);

    set_overview_progress(&mut f, &output, None);
    f.niri_state().move_cursor(Point::from((0., 0.)));
    assert_ne!(zoom_focal(&mut f, &output), before_focal);
}

#[test]
fn zoom_overview_actions_hold_and_multi_output_state_survive() {
    let mut f = set_up();
    let output1 = f.niri_output(1);
    f.add_output(2, (1920, 720));
    let output2 = f.niri_output(2);
    let center = Point::from((960., 360.));

    set_zoom(&mut f, &output1, 2., center);
    set_zoom(&mut f, &output2, 4., center);
    f.niri_state().move_cursor(center);
    f.niri().layout.toggle_overview();
    f.niri_complete_animations();

    assert_eq!(effective_zoom_transform(&mut f, &output1).factor(), 1.);
    assert_eq!(effective_zoom_transform(&mut f, &output2).factor(), 1.);

    let target_before = zoom_target_level(&mut f, &output1);
    f.niri_state().do_action(Action::ZoomIn, false);
    let target_after_action = zoom_target_level(&mut f, &output1);
    assert!(target_after_action > target_before);
    assert_eq!(effective_zoom_transform(&mut f, &output1).factor(), 1.);

    f.niri_state().do_action(Action::ZoomLock(false), false);
    assert!(zoom_locked(&mut f, &output1));
    let trigger = key_trigger(30);
    hold_press(&mut f, trigger, 3.);
    let target_after_hold = zoom_target_level(&mut f, &output1);
    assert_eq!(target_after_hold, 3.);
    assert_eq!(effective_zoom_transform(&mut f, &output1).factor(), 1.);
    hold_release(&mut f, trigger);
    assert_eq!(zoom_target_level(&mut f, &output1), target_after_action);

    f.niri().layout.toggle_overview();
    f.niri_complete_animations();
    assert_eq!(
        effective_zoom_transform(&mut f, &output1).factor(),
        target_after_action
    );
    assert_eq!(effective_zoom_transform(&mut f, &output2).factor(), 4.);
}

#[test]
fn zoom_overview_hides_zoom_redraw_and_completes_from_clock() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    let focal = Point::from((960., 360.));
    freeze_clock(&mut f);
    set_zoom(&mut f, &output, 1.5, focal);

    f.niri()
        .layout
        .monitor_for_output_mut(&output)
        .unwrap()
        .zoom_to(4., focal);
    assert!(zoom_is_animating(&mut f, &output));
    assert!(f.niri().layout.are_animations_ongoing(Some(&output)));

    set_overview_open(&mut f, &output, true);
    assert!(!f.niri().layout.are_animations_ongoing(Some(&output)));

    let now = f.niri().clock.now_unadjusted();
    f.niri().clock.set_rate(1.);
    f.niri().clock.set_unadjusted(now + Duration::from_secs(5));
    let _ = f.niri().clock.now();
    f.niri().clock.set_rate(0.);

    assert_eq!(zoom_level(&mut f, &output), 4.);
    assert!(!zoom_is_animating(&mut f, &output));
    assert_eq!(effective_zoom_transform(&mut f, &output).factor(), 1.);

    f.niri()
        .layout
        .monitor_for_output_mut(&output)
        .unwrap()
        .zoom_mut()
        .advance_animations();
    assert_eq!(zoom_target_level(&mut f, &output), 4.);
}

#[test]
fn zoom_overview_open_hot_corner_uses_identity_presentation() {
    let mut f = set_up();
    let output = f.niri_output(1);
    set_zoom(&mut f, &output, 4., Point::from((960., 360.)));
    set_overview_open(&mut f, &output, true);

    assert!(
        f.niri().contents_under(Point::from((0., 0.))).hot_corner,
        "fully-open Overview must evaluate hot corners in identity presentation"
    );
    assert!(f.niri().layout.is_overview_open());
}

#[test]
fn zoom_overview_output_add_and_remove_are_isolated() {
    let mut f = set_up();
    let output1 = f.niri_output(1);
    set_zoom(&mut f, &output1, 2., Point::from((960., 360.)));
    f.niri().layout.toggle_overview();
    f.niri_complete_animations();

    f.add_output(2, (1920, 720));
    let output2 = f.niri_output(2);
    assert_eq!(effective_zoom_transform(&mut f, &output1).factor(), 1.);
    assert_eq!(effective_zoom_transform(&mut f, &output2).factor(), 1.);

    f.niri().remove_output(&output2);
    assert_eq!(effective_zoom_transform(&mut f, &output1).factor(), 1.);

    f.niri().layout.toggle_overview();
    f.niri_complete_animations();
    assert_eq!(effective_zoom_transform(&mut f, &output1).factor(), 2.);
}

#[test]
fn zoom_overview_preserves_stored_transition_state() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    let focal = Point::from((960., 360.));
    freeze_clock(&mut f);
    set_zoom(&mut f, &output, 1.5, focal);

    f.niri()
        .layout
        .monitor_for_output_mut(&output)
        .unwrap()
        .zoom_to(4., focal);
    let stored_focal = zoom_focal(&mut f, &output);
    assert!(zoom_is_animating(&mut f, &output));

    set_overview_progress(&mut f, &output, Some(0.5));
    let partial = effective_zoom_transform(&mut f, &output).factor();
    assert!(partial > 1. && partial < 1.5);
    assert_eq!(zoom_target_level(&mut f, &output), 4.);
    assert_eq!(zoom_focal(&mut f, &output), stored_focal);

    advance_clock(&mut f, 100);
    assert!(zoom_is_animating(&mut f, &output));
    assert_ne!(effective_zoom_transform(&mut f, &output).factor(), partial);

    set_overview_open(&mut f, &output, true);
    assert_eq!(effective_zoom_transform(&mut f, &output).factor(), 1.);
    assert!(zoom_is_animating(&mut f, &output));
    assert_eq!(zoom_target_level(&mut f, &output), 4.);
    assert_eq!(zoom_focal(&mut f, &output), stored_focal);
}

#[test]
fn zoom_displayed_pointer_uses_owner_output_transform() {
    let mut f = set_up();
    f.add_output(2, (1920, 720));
    let output_a = f.niri_output(1);
    let output_b = f.niri_output(2);

    set_zoom(&mut f, &output_a, 2., Point::from((960., 360.)));
    set_zoom(&mut f, &output_b, 4., Point::from((960., 360.)));

    // The canonical point belongs to A, but its displayed position is over B.
    // B's transform must not be applied a second time.
    let canonical = Point::from((1450., 360.));
    assert_eq!(
        f.niri().display_position_for_content(canonical),
        Point::from((1940., 360.))
    );

    // A point without an owning output keeps the established raw fallback.
    let outside = Point::from((-50., -50.));
    assert_eq!(f.niri().display_position_for_content(outside), outside);
}

#[test]
fn zoom_pipewire_metadata_and_embedded_cursor_share_displayed_position() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let canonical = Point::from((150., 100.));
    f.niri_state().move_cursor(canonical);
    set_zoom(&mut f, &output, 2., Point::from((100., 100.)));

    // This is the output-local hotspot position passed to CursorData before
    // PipeWire serializes spa_meta_cursor.position.
    let metadata_position = f
        .niri()
        .pointer_pos_for_output_cast(&output)
        .expect("displayed cursor must be inside the cast output");
    assert_eq!(metadata_position, Point::from((200., 100.)));

    // The embedded render path uses the same displayed hotspot, with only the
    // unscaled cursor hotspot applied to the sprite's top-left geometry.
    let locs = pointer_element_locs(f.niri_state(), &output);
    assert_eq!(locs.len(), 1);
    let hotspot = cursor_hotspot(&mut f, &output);
    let scale = Scale::from(output.current_scale().fractional_scale());
    assert_eq!(
        locs[0],
        (metadata_position - hotspot).to_physical_precise_round(scale)
    );

    f.niri().pointer_visibility = crate::niri::PointerVisibility::Hidden;
    assert_eq!(f.niri().pointer_pos_for_output_cast(&output), None);
}

#[test]
fn zoom_image_copy_cursor_position_and_overlap_use_displayed_geometry() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let geo = f.niri().global_space.output_geometry(&output).unwrap();
    let mode = output.current_mode().unwrap();
    let cursor_size = Size::from((32, 32));
    let hotspot = Point::from((16, 16));

    f.niri_state().move_cursor(Point::from((476., 230.)));
    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    // Canonical (476, 230) displays at (-8, 100). The image overlaps the
    // output even though the hotspot itself is outside its left edge.
    assert_eq!(
        f.niri()
            .image_copy_cursor_pos(&output, geo, mode, cursor_size, hotspot),
        Some(Point::from((-8, 100)))
    );

    // A canonical pointer just outside the output still enters the capture
    // while its displayed cursor image overlaps the output edge.
    f.niri_state().move_cursor(Point::from((-10., 10.)));
    assert_eq!(
        f.niri()
            .image_copy_cursor_pos(&output, geo, mode, cursor_size, hotspot),
        Some(Point::from((-10, 10)))
    );
}

#[test]
fn zoom_color_picker_samples_the_displayed_framebuffer() {
    let mut f = set_up();
    assert_llvmpipe(f.niri_state());

    let output = f.niri_output(1);
    let id = f.add_client();
    open_window(
        &mut f,
        id,
        "zoom-color-picker",
        400,
        300,
        [0x40404040, 0x80808080, 0xc0c0c0c0, 0xffffffff],
    );
    let output_origin = f
        .niri()
        .global_space
        .output_geometry(&output)
        .unwrap()
        .loc
        .to_f64();
    let window_geo = f
        .niri()
        .layout
        .windows_for_output(&output)
        .next()
        .unwrap()
        .window
        .geometry();
    let canonical =
        output_origin + window_geo.loc.to_f64() + window_geo.size.to_f64().downscale(2.);

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));
    let picked = crate::input::pick_color_grab::PickColorGrab::pick_color_at_point(
        canonical,
        f.niri_state(),
    )
    .expect("color picker must read the rendered pixel");

    assert_abs_diff_eq!(picked.rgb[0], 0x40 as f64 / 255., epsilon = 0.02);
    assert_abs_diff_eq!(picked.rgb[1], 0x80 as f64 / 255., epsilon = 0.02);
    assert_abs_diff_eq!(picked.rgb[2], 0xc0 as f64 / 255., epsilon = 0.02);
}

#[test]
fn zoom_screenshot_ui_pointer_down_uses_displayed_coordinates() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let id = f.add_client();
    open_window(
        &mut f,
        id,
        "zoom-screenshot-ui",
        400,
        300,
        [0xff, 0, 0, 0xff],
    );
    set_zoom(&mut f, &output, 2., Point::from((100., 100.)));
    f.niri_state().open_screenshot_ui(true, None);

    let canonical = Point::from((150., 100.));
    let displayed = f.niri().display_position_for_content(canonical);
    let point = (displayed
        - f.niri()
            .global_space
            .output_geometry(&output)
            .unwrap()
            .loc
            .to_f64())
    .to_physical(output.current_scale().fractional_scale())
    .to_i32_round();

    assert!(f
        .niri()
        .screenshot_ui
        .pointer_down(output.clone(), point, None, false));
    f.niri().screenshot_ui.pointer_motion(point, None);
    let (_, end) = f
        .niri()
        .screenshot_ui
        .selection_points()
        .expect("screenshot UI must remain in pointer-down state");
    assert_eq!(end, point);
}

#[test]
fn zoom_pointer_surface_outputs_follow_displayed_bbox() {
    let mut f = set_up();
    f.add_output(2, (1920, 720));
    let output1 = f.niri_output(1);
    let id = f.add_client();
    open_window(&mut f, id, "cursor-surface", 400, 300, [0xff, 0, 0, 0xff]);
    f.niri_state().move_cursor(Point::from((150., 100.)));
    f.roundtrip(id);

    let surface = f.client(id).create_cursor_surface(24, 24);
    surface.commit();
    f.client(id).set_cursor(Some(&surface), (0, 0));
    f.roundtrip(id);

    set_zoom(&mut f, &output1, 2., Point::from((960., 360.)));
    // Lock the zoom so the displayed pointer can straddle the outputs without
    // the deadzone follow moving the camera.
    f.niri()
        .layout
        .monitor_for_output_mut(&output1)
        .unwrap()
        .zoom_mut()
        .set_locked(true);
    f.niri().tablet_cursor_location = Some(Point::from((1430., 360.)));
    f.dispatch();
    f.double_roundtrip(id);

    let outputs = f.client(id).surface_outputs(&surface);
    assert!(
        outputs.iter().any(|name| name == "headless-1"),
        "outputs={outputs:?}"
    );
    assert!(
        outputs.iter().any(|name| name == "headless-2"),
        "outputs={outputs:?}"
    );
}

#[test]
fn zoom_overview_identity_reaches_image_copy_cursor_position() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let geo = f.niri().global_space.output_geometry(&output).unwrap();
    let mode = output.current_mode().unwrap();
    let canonical = Point::from((150., 100.));
    set_zoom(&mut f, &output, 4., Point::from((100., 100.)));
    set_overview_progress(&mut f, &output, Some(1.));
    f.niri_state().move_cursor(canonical);

    assert_eq!(
        f.niri().image_copy_cursor_pos(
            &output,
            geo,
            mode,
            Size::from((32, 32)),
            Point::from((16, 16)),
        ),
        Some(Point::from((150, 100)))
    );
}

#[test]
fn zoom_pipewire_metadata_updates_when_effective_transform_changes() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let canonical = Point::from((150., 100.));
    f.niri_state().move_cursor(canonical);
    set_zoom(&mut f, &output, 2., Point::from((100., 100.)));

    let before = f.niri().pointer_pos_for_output_cast(&output);
    set_overview_progress(&mut f, &output, Some(1.));
    let after = f.niri().pointer_pos_for_output_cast(&output);

    assert_eq!(before, Some(Point::from((200., 100.))));
    assert_eq!(after, Some(Point::from((150., 100.))));
    assert_ne!(before, after, "metadata must follow transform-only updates");
}

#[test]
fn zoom_window_cast_pointer_remains_canonical() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let id = f.add_client();
    open_window(&mut f, id, "window-cast", 400, 300, [0xff, 0, 0, 0xff]);
    let canonical = Point::from((150., 100.));
    f.niri_state().move_cursor(canonical);
    set_zoom(&mut f, &output, 2., Point::from((100., 100.)));

    let niri = f.niri();
    let mapped = niri.layout.windows_for_output(&output).next().unwrap();
    assert_eq!(
        niri.pointer_pos_for_window_cast(mapped).map(|(pos, _)| pos),
        Some(canonical)
    );
}

#[test]
fn zoom_pointer_presentation_transform_unlocked_identity() {
    let mut f = set_up();
    let output = f.niri_output(1);

    // At level 1 the stored transform keeps the output-center focal but is
    // semantically the identity; the pointer transform must equal the
    // effective transform and map every point to itself.
    assert!(!f.niri().is_locked());
    let transform = f.niri().pointer_presentation_transform(&output);
    assert_eq!(transform, effective_zoom_transform(&mut f, &output));
    assert_eq!(transform.factor(), 1.);
    assert_eq!(
        transform.apply(Point::from((150., 100.))),
        Point::from((150., 100.))
    );
}

#[test]
fn zoom_pointer_presentation_transform_unlocked_zoom() {
    let mut f = set_up();
    let output = f.niri_output(1);

    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));

    assert_eq!(
        f.niri().pointer_presentation_transform(&output),
        effective_zoom_transform(&mut f, &output)
    );
}

#[test]
fn zoom_pointer_presentation_transform_locked_is_identity() {
    // A real LockState needs live session-lock objects, so the locked branch is
    // covered through the pure policy helper that every presentation path
    // delegates to.
    let effective = crate::utils::view::ViewportTransform::new(Point::from((960., 360.)), 3.);
    assert_eq!(
        crate::niri::Niri::pointer_transform_for_presentation(true, effective),
        crate::utils::view::ViewportTransform::identity()
    );
}

#[test]
fn zoom_pointer_presentation_transform_locked_ignores_zoom_animation() {
    // A stored zoom transition may keep running under the lock; the pointer
    // presentation must stay identity at every animated level.
    for factor in [1.2, 1.8, 2.5] {
        let effective =
            crate::utils::view::ViewportTransform::new(Point::from((960., 360.)), factor);
        assert_eq!(
            crate::niri::Niri::pointer_transform_for_presentation(true, effective),
            crate::utils::view::ViewportTransform::identity()
        );
    }
}

#[test]
fn zoom_pointer_presentation_lock_entry_and_exit() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let canonical = Point::from((150., 100.));
    set_zoom(&mut f, &output, 3., Point::from((960., 360.)));

    // Unlocked: the pointer is displayed through the effective zoom transform.
    let effective = effective_zoom_transform(&mut f, &output);
    let zoomed_display = f.niri().display_position_for_content(canonical);
    assert_eq!(zoomed_display, effective.apply(canonical));
    assert_ne!(zoomed_display, canonical);

    // Locked: the lock surface is the authoritative presentation, so the
    // pointer is displayed at its canonical position.
    let locked = crate::niri::Niri::pointer_transform_for_presentation(true, effective);
    assert_eq!(locked.apply(canonical), canonical);

    // Unlock restores the desktop presentation; the stored zoom never changed.
    assert_eq!(zoom_transform(&mut f, &output), effective);
    assert_eq!(
        f.niri().display_position_for_content(canonical),
        zoomed_display
    );
}

#[test]
fn zoom_pointer_presentation_transform_overview_partial() {
    let mut f = set_up();
    let output = f.niri_output(1);
    set_zoom(&mut f, &output, 4., Point::from((960., 360.)));
    set_overview_progress(&mut f, &output, Some(0.5));

    // Unlocked, the pointer follows the effective (Overview-suppressed) transform.
    let effective = effective_zoom_transform(&mut f, &output);
    assert!(effective.factor() < 4.);
    assert_eq!(f.niri().pointer_presentation_transform(&output), effective);
}

#[test]
fn zoom_pointer_presentation_transform_overview_open() {
    let mut f = set_up();
    let output = f.niri_output(1);
    set_zoom(&mut f, &output, 4., Point::from((960., 360.)));
    set_overview_open(&mut f, &output, true);

    assert_eq!(
        f.niri().pointer_presentation_transform(&output),
        crate::utils::view::ViewportTransform::identity()
    );
}

#[test]
fn zoom_pointer_presentation_transform_uses_owner_output() {
    let mut f = set_up();
    f.add_output(2, (1920, 720));
    let output_a = f.niri_output(1);
    let output_b = f.niri_output(2);

    set_zoom(&mut f, &output_a, 2., Point::from((960., 360.)));
    set_zoom(&mut f, &output_b, 4., Point::from((960., 360.)));

    assert_eq!(
        f.niri().pointer_presentation_transform(&output_a).factor(),
        2.
    );
    assert_eq!(
        f.niri().pointer_presentation_transform(&output_b).factor(),
        4.
    );
}

#[test]
fn zoom_session_lock_relative_factor_is_identity() {
    // Relative pointer deltas are scaled by the pointer presentation factor.
    // While the session is locked the presentation is the identity, so a
    // stored zoom level must not scale the deltas.
    let effective = crate::utils::view::ViewportTransform::new(Point::from((960., 360.)), 4.);
    let locked = crate::niri::Niri::pointer_transform_for_presentation(true, effective);
    assert_eq!(locked.factor(), 1.);

    let unlocked = crate::niri::Niri::pointer_transform_for_presentation(false, effective);
    assert_eq!(unlocked.factor(), 4.);
}

#[test]
fn zoom_session_lock_absolute_inverse_is_identity() {
    // Absolute pointer, touch and output-mapped tablet positions are converted
    // through the inverse of the pointer presentation transform. While the
    // session is locked the inverse is the identity: a display position maps
    // to the raw lock coordinate.
    let effective = crate::utils::view::ViewportTransform::new(Point::from((960., 360.)), 4.);
    let locked = crate::niri::Niri::pointer_transform_for_presentation(true, effective);

    let display = Point::from((1234., 567.));
    assert_eq!(locked.apply_inverse(display), display);

    // Unlocked regression: the inverse still maps through the stored zoom.
    let unlocked = crate::niri::Niri::pointer_transform_for_presentation(false, effective);
    assert_eq!(
        unlocked.apply_inverse(display),
        Point::from((960. + (1234. - 960.) / 4., 360. + (567. - 360.) / 4.))
    );
}

#[test]
fn zoom_session_lock_presentation_viewport_is_full_output() {
    // The zoom-lock pointer clamp derives its viewport from the pointer
    // presentation transform. While the session is locked the transform is
    // the identity, so the presented viewport is the entire output and the
    // pointer is not confined to the hidden zoomed viewport.
    let mut f = set_up();
    let output = f.niri_output(1);
    set_zoom(&mut f, &output, 4., Point::from((960., 360.)));

    let view_size = f
        .niri()
        .layout
        .monitor_for_output(&output)
        .unwrap()
        .view_size();
    let output_rect = Rectangle::from_size(view_size);

    let effective = effective_zoom_transform(&mut f, &output);
    let locked = crate::niri::Niri::pointer_transform_for_presentation(true, effective);
    assert_eq!(locked.apply_inverse_rect(output_rect), output_rect);

    // Unlocked regression: the presented viewport stays the zoomed viewport.
    let unlocked = crate::niri::Niri::pointer_transform_for_presentation(false, effective);
    let zoomed_viewport = unlocked.apply_inverse_rect(output_rect);
    assert_eq!(zoomed_viewport.size, view_size.downscale(4.));
    assert_ne!(zoomed_viewport, output_rect);
}

#[test]
fn zoom_session_lock_deadzone_tracking_suspended() {
    // Pointer motion over the lock surface must not move the hidden zoom
    // camera: focal tracking is suspended while the session is locked.
    let gates = |session_locked, overview_active| crate::input::ZoomTrackingGates {
        session_locked,
        overview_active,
        screenshot_ui_open: false,
        mru_active: false,
    };
    assert!(!crate::input::zoom_tracking_enabled(gates(true, false)));
    assert!(!crate::input::zoom_tracking_enabled(gates(true, true)));

    // Regressions: the Overview still suspends tracking, and normal desktop
    // tracking stays enabled.
    assert!(!crate::input::zoom_tracking_enabled(gates(false, true)));
    assert!(crate::input::zoom_tracking_enabled(gates(false, false)));
}

#[test]
fn zoom_session_lock_warp_policy_uses_presentation() {
    // Programmatic warps share the zoom-lock clamp and the deadzone tracking
    // helpers. Under the locked presentation the clamp viewport is the entire
    // output and tracking is suspended, so a warp neither clamps to the
    // hidden zoomed viewport nor mutates the stored focal.
    let mut f = set_up();
    let output = f.niri_output(1);
    set_zoom(&mut f, &output, 4., Point::from((960., 360.)));

    let view_size = f
        .niri()
        .layout
        .monitor_for_output(&output)
        .unwrap()
        .view_size();
    let effective = effective_zoom_transform(&mut f, &output);
    let locked = crate::niri::Niri::pointer_transform_for_presentation(true, effective);
    let viewport = locked.apply_inverse_rect(Rectangle::from_size(view_size));

    let candidate = Point::<f64, Logical>::from((1900., 700.));
    assert!(viewport.contains(candidate));
    assert!(!crate::input::zoom_tracking_enabled(
        crate::input::ZoomTrackingGates {
            session_locked: true,
            overview_active: false,
            screenshot_ui_open: false,
            mru_active: false,
        }
    ));
}

#[test]
fn zoom_session_lock_multi_output_presentation_is_identity() {
    // Under a session lock every output presents at the identity regardless of
    // its stored zoom level.
    let effective_a = crate::utils::view::ViewportTransform::new(Point::from((960., 360.)), 2.);
    let effective_b = crate::utils::view::ViewportTransform::new(Point::from((960., 360.)), 4.);
    assert_eq!(
        crate::niri::Niri::pointer_transform_for_presentation(true, effective_a),
        crate::utils::view::ViewportTransform::identity()
    );
    assert_eq!(
        crate::niri::Niri::pointer_transform_for_presentation(true, effective_b),
        crate::utils::view::ViewportTransform::identity()
    );
}

#[test]
fn zoom_session_lock_unlock_restores_effective_transform() {
    // Locking and unlocking only switches the presentation policy; the stored
    // zoom state is untouched.
    let effective = crate::utils::view::ViewportTransform::new(Point::from((960., 360.)), 4.);
    assert_eq!(
        crate::niri::Niri::pointer_transform_for_presentation(true, effective),
        crate::utils::view::ViewportTransform::identity()
    );
    assert_eq!(
        crate::niri::Niri::pointer_transform_for_presentation(false, effective),
        effective
    );
}

#[test]
fn zoom_color_picker_uses_presentation_transform() {
    // The color picker samples the displayed framebuffer at the position the
    // pointer presentation transform maps to. While the session is locked
    // that transform is the identity, so the sample lands on the lock
    // framebuffer pixel under the visible cursor.
    let local = Point::from((1234., 567.));
    let effective = crate::utils::view::ViewportTransform::new(Point::from((960., 360.)), 4.);

    let locked = crate::niri::Niri::pointer_transform_for_presentation(true, effective);
    assert_eq!(locked.apply(local), local);

    // Unlocked regression: sampling still follows the effective zoom.
    let unlocked = crate::niri::Niri::pointer_transform_for_presentation(false, effective);
    assert_eq!(unlocked.apply(local), effective.apply(local));
    assert_ne!(unlocked.apply(local), local);
}

const MRU_CONFIG: &str = r#"
animations {
    off
}

hotkey-overlay {
    skip-at-startup
}

recent-windows {
    open-delay-ms 0
}

layout {
    gaps 0
}
"#;

/// Opens the MRU UI on the active output and lets its view settle.
fn open_mru(f: &mut Fixture) {
    f.niri_state().do_action(
        Action::MruAdvance {
            direction: MruDirection::Forward,
            scope: None,
            filter: None,
        },
        false,
    );
    assert!(f.niri().window_mru_ui.is_open());
    // With animations off the view position still needs a couple of advance
    // steps to reach its target.
    for _ in 0..3 {
        f.niri().advance_animations();
    }
}

const MRU_ANIMATED_CONFIG: &str = r#"
hotkey-overlay {
    skip-at-startup
}

recent-windows {
    open-delay-ms 0
}

layout {
    gaps 0
}
"#;

/// The MRU thumbnail under an output-local displayed position.
fn mru_thumbnail_at(
    f: &mut Fixture,
    pos_within_output: Point<f64, Logical>,
) -> Option<crate::window::mapped::MappedId> {
    f.niri().window_mru_ui.pointer_motion(pos_within_output)
}

/// Sends a left mouse button press through the virtual pointer protocol.
fn click_left(f: &mut Fixture, id: ClientId) {
    const BTN_LEFT: u32 = 0x110;
    let client = f.client(id);
    let manager = client.state.virtual_pointer_manager.as_ref().unwrap();
    let pointer = manager.create_virtual_pointer(None, &client.qh, ());
    pointer.button(0, BTN_LEFT, wl_pointer::ButtonState::Pressed.into());
    f.roundtrip(id);
}

/// Sends a left mouse button release through the virtual pointer protocol.
fn release_left(f: &mut Fixture, id: ClientId) {
    const BTN_LEFT: u32 = 0x110;
    let client = f.client(id);
    let manager = client.state.virtual_pointer_manager.as_ref().unwrap();
    let pointer = manager.create_virtual_pointer(None, &client.qh, ());
    pointer.button(0, BTN_LEFT, wl_pointer::ButtonState::Released.into());
    f.roundtrip(id);
}

/// Finds a displayed position over an MRU thumbnail whose canonical pre-image
/// under `transform` hit-tests differently, so that a canonical-coordinate
/// click would produce an observably different result.
///
/// Returns `(displayed, thumbnail_id)` or `None` when no distinguishing
/// position exists.
fn mru_distinguishing_position(
    f: &mut Fixture,
    origin: Point<f64, Logical>,
    transform: crate::utils::view::ViewportTransform,
    width: f64,
) -> Option<(Point<f64, Logical>, crate::window::mapped::MappedId)> {
    // The clicked thumbnail must not be the already-focused window, otherwise
    // a buggy canonical-coordinate click that cancels the MRU would leave the
    // same focus and the test could not tell the two paths apart.
    let focus_before = f.niri().layout.focus().map(|m| m.id());
    let mut x = 0.;
    while x < width {
        let d = Point::from((x, 360.));
        if let Some(id_d) = mru_thumbnail_at(f, d) {
            let c = origin + transform.apply_inverse(d - origin);
            if mru_thumbnail_at(f, c) != Some(id_d) && Some(id_d) != focus_before {
                return Some((d, id_d));
            }
        }
        x += 4.;
    }
    None
}

#[test]
fn zoom_mru_button_1x() {
    let mut f = set_up_with_config(MRU_CONFIG);
    let id = f.add_client();
    open_window(&mut f, id, "one", 100, 100, [255, 0, 0, 255]);
    open_window(&mut f, id, "two", 100, 100, [0, 255, 0, 255]);
    open_mru(&mut f);
    // At 1x canonical == displayed; clicking a thumbnail confirms it. Pick a
    // thumbnail that is not the already-focused window so that a cancel would
    // leave observably different focus.
    let focus_before = f.niri().layout.focus().map(|m| m.id());
    let mut target = None;
    let mut x = 0.;
    while x < 1920. {
        let d = Point::from((x, 360.));
        if let Some(id_d) = mru_thumbnail_at(&mut f, d) {
            if Some(id_d) != focus_before {
                target = Some((d, id_d));
                break;
            }
        }
        x += 4.;
    }
    let (d, id_d) = target.expect("no MRU thumbnail found");

    f.niri_state().move_cursor(d);
    click_left(&mut f, id);

    assert!(!f.niri().window_mru_ui.is_open());
    assert_eq!(f.niri().layout.focus().map(|m| m.id()), Some(id_d));
}

#[test]
fn zoom_mru_button_2x() {
    let mut f = set_up_with_config(MRU_CONFIG);
    let output = f.niri_output(1);
    let id = f.add_client();
    open_window(&mut f, id, "one", 100, 100, [255, 0, 0, 255]);
    open_window(&mut f, id, "two", 100, 100, [0, 255, 0, 255]);
    open_mru(&mut f);

    // The canonical pointer C maps to the displayed position D over a
    // thumbnail. The click must hit-test at D, not at C.
    let focal = Point::from((480., 360.));
    let transform = crate::utils::view::ViewportTransform::new(focal, 2.);
    let (d, id_d) = mru_distinguishing_position(&mut f, Point::from((0., 0.)), transform, 1920.)
        .expect("no distinguishing MRU position found");

    let c = transform.apply_inverse(d);
    f.niri_state().move_cursor(c);
    set_zoom(&mut f, &output, 2., focal);
    let displayed = f.niri().display_position_for_content(c);
    assert_abs_diff_eq!(displayed.x, d.x, epsilon = EPS);
    assert_abs_diff_eq!(displayed.y, d.y, epsilon = EPS);

    click_left(&mut f, id);

    assert!(!f.niri().window_mru_ui.is_open());
    assert_eq!(f.niri().layout.focus().map(|m| m.id()), Some(id_d));
}

#[test]
fn zoom_mru_button_matches_motion_position() {
    let mut f = set_up_with_config(MRU_CONFIG);
    let output = f.niri_output(1);
    let id = f.add_client();
    open_window(&mut f, id, "one", 100, 100, [255, 0, 0, 255]);
    open_window(&mut f, id, "two", 100, 100, [0, 255, 0, 255]);
    open_mru(&mut f);

    let focal = Point::from((480., 360.));
    let transform = crate::utils::view::ViewportTransform::new(focal, 2.);
    let (d, id_d) = mru_distinguishing_position(&mut f, Point::from((0., 0.)), transform, 1920.)
        .expect("no distinguishing MRU position found");

    set_zoom(&mut f, &output, 2., focal);

    // Pointer motion selects the thumbnail under the displayed position.
    let extent = f.niri().global_space.output_geometry(&output).unwrap().size;
    move_pointer_absolute(&mut f, id, d, extent, None);
    assert_eq!(f.niri().window_mru_ui.current_window_id(), Some(id_d));

    // The button must hit-test at the same displayed position.
    click_left(&mut f, id);

    assert!(!f.niri().window_mru_ui.is_open());
    assert_eq!(f.niri().layout.focus().map(|m| m.id()), Some(id_d));
}

#[test]
fn zoom_mru_button_animated_zoom() {
    let mut f = set_up_with_config(MRU_ANIMATED_CONFIG);
    let output = f.niri_output(1);
    let id = f.add_client();
    open_window(&mut f, id, "one", 100, 100, [255, 0, 0, 255]);
    open_window(&mut f, id, "two", 100, 100, [0, 255, 0, 255]);

    f.niri_state().move_cursor(Point::from((960., 360.)));
    open_mru(&mut f);

    // Start a zoom transition and freeze it mid-flight.
    freeze_clock(&mut f);
    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(4.)), false);
    advance_clock(&mut f, 100);
    assert!(zoom_is_animating(&mut f, &output));

    // The displayed position is derived from the current transition-aware
    // presentation transform, not the resting or target level.
    let origin = f
        .niri()
        .global_space
        .output_geometry(&output)
        .unwrap()
        .loc
        .to_f64();
    let transform = f.niri().pointer_presentation_transform(&output);
    assert_ne!(transform.factor(), 1.);
    assert_ne!(transform.factor(), 4.);

    let (d, id_d) = mru_distinguishing_position(&mut f, origin, transform, 1920.)
        .expect("no distinguishing MRU position found");

    // Place the canonical pointer so that its displayed position is D. Use
    // set_location to avoid perturbing the in-flight transition's focal.
    let c = origin + transform.apply_inverse(d - origin);
    f.niri().seat.get_pointer().unwrap().set_location(c);
    let displayed = f.niri().display_position_for_content(c);
    assert_abs_diff_eq!(displayed.x, d.x, epsilon = EPS);
    assert_abs_diff_eq!(displayed.y, d.y, epsilon = EPS);

    click_left(&mut f, id);

    assert!(!f.niri().window_mru_ui.is_open());
    assert_eq!(f.niri().layout.focus().map(|m| m.id()), Some(id_d));
}

#[test]
fn zoom_mru_button_overview_uses_effective_transform() {
    let mut f = set_up_with_config(MRU_CONFIG);
    let output = f.niri_output(1);
    let id = f.add_client();
    open_window(&mut f, id, "one", 100, 100, [255, 0, 0, 255]);
    open_window(&mut f, id, "two", 100, 100, [0, 255, 0, 255]);
    open_mru(&mut f);

    // The MRU UI can coexist with the Overview; the pointer presentation
    // follows the effective (Overview-suppressed) transform.
    let focal = Point::from((960., 360.));
    set_zoom(&mut f, &output, 4., focal);
    set_overview_progress(&mut f, &output, Some(0.5));

    let effective = f.niri().pointer_presentation_transform(&output);
    assert_abs_diff_eq!(effective.factor(), 2., epsilon = EPS);
    let stored = zoom_transform(&mut f, &output);
    assert_abs_diff_eq!(stored.factor(), 4., epsilon = EPS);

    let origin = f
        .niri()
        .global_space
        .output_geometry(&output)
        .unwrap()
        .loc
        .to_f64();
    let (d, id_d) = mru_distinguishing_position(&mut f, origin, effective, 1920.)
        .expect("no distinguishing MRU position found");

    let c = origin + effective.apply_inverse(d - origin);
    f.niri().seat.get_pointer().unwrap().set_location(c);
    let displayed = f.niri().display_position_for_content(c);
    assert_abs_diff_eq!(displayed.x, d.x, epsilon = EPS);
    assert_abs_diff_eq!(displayed.y, d.y, epsilon = EPS);

    click_left(&mut f, id);

    assert!(!f.niri().window_mru_ui.is_open());
    assert_eq!(f.niri().layout.focus().map(|m| m.id()), Some(id_d));
}

#[test]
fn zoom_mru_button_multi_output_displayed_target() {
    let mut f = set_up_with_config(MRU_CONFIG);
    f.add_output(2, (1920, 720));
    let output_a = f.niri_output(1);
    let output_b = f.niri_output(2);
    let id = f.add_client();
    open_window(&mut f, id, "one", 100, 100, [255, 0, 0, 255]);
    open_window(&mut f, id, "two", 100, 100, [0, 255, 0, 255]);

    // The MRU opens on the active output B.
    f.niri().layout.focus_output(&output_b);
    open_mru(&mut f);
    assert_eq!(f.niri().window_mru_ui.output(), Some(&output_b));

    // Zoom output A so that a canonical position on A displays on B.
    let origin_a = f
        .niri()
        .global_space
        .output_geometry(&output_a)
        .unwrap()
        .loc
        .to_f64();
    let origin_b = f
        .niri()
        .global_space
        .output_geometry(&output_b)
        .unwrap()
        .loc
        .to_f64();
    let focal_a = Point::from((1200., 360.));
    set_zoom(&mut f, &output_a, 4., focal_a);

    // Find a thumbnail on B whose canonical pre-image stays on A.
    let transform = crate::utils::view::ViewportTransform::new(focal_a, 4.);
    let mut target = None;
    let mut x = 0.;
    while x < 1920. {
        let d_local = Point::from((x, 360.));
        if let Some(id_d) = mru_thumbnail_at(&mut f, d_local) {
            let d_global = origin_b + d_local;
            let c = origin_a + transform.apply_inverse(d_global - origin_a);
            if let Some((owner, _)) = f.niri().output_under(c) {
                if owner == &output_a {
                    target = Some((d_global, c, id_d));
                    break;
                }
            }
        }
        x += 4.;
    }
    let (d_global, c, id_d) = target.expect("no cross-output MRU position found");

    f.niri().seat.get_pointer().unwrap().set_location(c);
    let displayed = f.niri().display_position_for_content(c);
    assert_abs_diff_eq!(displayed.x, d_global.x, epsilon = EPS);
    assert_abs_diff_eq!(displayed.y, d_global.y, epsilon = EPS);

    click_left(&mut f, id);

    assert!(!f.niri().window_mru_ui.is_open());
    assert_eq!(f.niri().layout.focus().map(|m| m.id()), Some(id_d));
}

#[test]
fn zoom_mru_button_displayed_off_output_cancels() {
    let mut f = set_up_with_config(MRU_CONFIG);
    let output = f.niri_output(1);
    let id = f.add_client();
    open_window(&mut f, id, "one", 100, 100, [255, 0, 0, 255]);
    open_window(&mut f, id, "two", 100, 100, [0, 255, 0, 255]);
    open_mru(&mut f);

    let focus_before = f.niri().layout.focus().map(|m| m.id());

    // A canonical position near the right edge displays past the output at
    // 2x. The click must cancel the MRU, not panic or hit-test at C.
    let focal = Point::from((960., 360.));
    set_zoom(&mut f, &output, 2., focal);
    let c = Point::from((1900., 360.));
    let d = f.niri().display_position_for_content(c);
    assert!(d.x > 1920.);
    assert!(f.niri().output_under(d).is_none());

    f.niri().seat.get_pointer().unwrap().set_location(c);
    click_left(&mut f, id);

    assert!(!f.niri().window_mru_ui.is_open());
    assert_eq!(f.niri().layout.focus().map(|m| m.id()), focus_before);

    release_left(&mut f, id);
    // A canonical position off every output displays unchanged, also off
    // every output. The click must cancel the MRU rather than unwrap-panic.
    open_mru(&mut f);
    let c = Point::from((2000., 360.));
    let d = f.niri().display_position_for_content(c);
    assert_eq!(d, c);
    assert!(f.niri().output_under(d).is_none());

    f.niri().seat.get_pointer().unwrap().set_location(c);
    click_left(&mut f, id);

    assert!(!f.niri().window_mru_ui.is_open());
    assert_eq!(f.niri().layout.focus().map(|m| m.id()), focus_before);
}

// --- Touchpad pinch zoom ---------------------------------------------------

fn set_up_with_pinch() -> Fixture {
    set_up_with_zoom("zoom { pinch-fingers 3; }")
}

fn zoom_pinch(f: &mut Fixture) -> Option<ZoomPinchRouting> {
    f.niri().zoom_pinch.clone()
}

fn zoom_gesturing(f: &mut Fixture, output: &Output) -> bool {
    f.niri()
        .layout
        .monitor_for_output(output)
        .unwrap()
        .zoom()
        .is_gesturing()
}

fn zoom_animating(f: &mut Fixture, output: &Output) -> bool {
    f.niri()
        .layout
        .monitor_for_output(output)
        .unwrap()
        .zoom()
        .is_animating()
}

#[test]
fn zoom_pinch_claim_requires_config() {
    // Pinch zoom is disabled by default: no finger count may be claimed.
    let mut f = set_up();
    assert!(!f.niri_state().can_claim_zoom_pinch(2));
    assert!(!f.niri_state().can_claim_zoom_pinch(3));
    assert!(!f.niri_state().can_claim_zoom_pinch(4));
}

#[test]
fn zoom_pinch_claim_finger_match() {
    let mut f = set_up_with_pinch();
    assert!(f.niri_state().can_claim_zoom_pinch(3));
    assert!(!f.niri_state().can_claim_zoom_pinch(2));
    assert!(!f.niri_state().can_claim_zoom_pinch(4));
}

#[test]
fn zoom_pinch_claim_gates() {
    use crate::input::{zoom_pinch_claim_allowed, ZoomPinchGates};

    let clear = ZoomPinchGates {
        pinch_fingers: Some(3),
        fingers: 3,
        session_locked: false,
        screenshot_ui_open: false,
        mru_open: false,
        pointer_grabbed: false,
        overview_active: false,
    };
    assert!(zoom_pinch_claim_allowed(clear));

    // Every gate independently blocks the claim.
    for gates in [
        ZoomPinchGates {
            pinch_fingers: None,
            ..clear
        },
        ZoomPinchGates {
            pinch_fingers: Some(2),
            ..clear
        },
        ZoomPinchGates {
            fingers: 2,
            ..clear
        },
        ZoomPinchGates {
            session_locked: true,
            ..clear
        },
        ZoomPinchGates {
            screenshot_ui_open: true,
            ..clear
        },
        ZoomPinchGates {
            mru_open: true,
            ..clear
        },
        ZoomPinchGates {
            pointer_grabbed: true,
            ..clear
        },
        ZoomPinchGates {
            overview_active: true,
            ..clear
        },
    ] {
        assert!(!zoom_pinch_claim_allowed(gates), "{gates:?}");
    }
}

#[test]
fn zoom_pinch_claim_blocked_by_screenshot_ui() {
    let mut f = set_up_with_pinch();
    f.niri_state().open_screenshot_ui(true, None);
    assert!(f.niri().screenshot_ui.is_open());

    assert!(!f.niri_state().can_claim_zoom_pinch(3));
}

#[test]
fn zoom_pinch_claim_blocked_by_mru() {
    let mut f = set_up_with_config(&format!("{MRU_CONFIG}\nzoom {{ pinch-fingers 3; }}"));
    let id = f.add_client();
    open_window(&mut f, id, "one", 100, 100, [255, 0, 0, 255]);
    open_mru(&mut f);

    assert!(!f.niri_state().can_claim_zoom_pinch(3));
}

#[test]
fn zoom_pinch_claim_blocked_by_overview() {
    let mut f = set_up_with_pinch();
    f.niri_state().move_cursor(Point::from((150., 100.)));
    f.niri().layout.toggle_overview();

    assert!(!f.niri_state().can_claim_zoom_pinch(3));
}

#[test]
fn zoom_pinch_claim_requires_target_output() {
    // With no outputs there is no zoom target, so a matching pinch cannot be
    // claimed: the begin must stay client-owned, or the client would receive
    // update/end events for a begin it never saw.
    let mut f = set_up_with_pinch();
    let output = f.niri_output(1);
    f.niri().remove_output(&output);
    assert!(f.niri().layout.active_output().is_none());

    assert!(!f.niri_state().can_claim_zoom_pinch(3));
    assert!(zoom_pinch(&mut f).is_none());
}

#[test]
fn zoom_pinch_begin_claims() {
    let mut f = set_up_with_pinch();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri_state().begin_zoom_pinch("dev0".to_owned());

    assert!(matches!(
        zoom_pinch(&mut f),
        Some(ZoomPinchRouting::Active {
            output: ref owner,
            device_id: ref dev,
        }) if *owner == output && dev == "dev0"
    ));
    assert!(zoom_gesturing(&mut f, &output));
    assert!(!zoom_animating(&mut f, &output));
}

#[test]
fn zoom_pinch_update_drives_level() {
    let mut f = set_up_with_pinch();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri_state().begin_zoom_pinch("dev0".to_owned());
    f.niri_state().update_zoom_pinch(&output, 2.);

    assert_eq!(zoom_level(&mut f, &output), 2.);
    assert_eq!(zoom_target_level(&mut f, &output), 2.);
    assert!(zoom_gesturing(&mut f, &output));
    // Updates never allocate an animation.
    assert!(!zoom_animating(&mut f, &output));
}

#[test]
fn zoom_pinch_update_clamps() {
    let mut f = set_up_with_zoom("zoom { pinch-fingers 3; max-zoom 4; }");
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri_state().begin_zoom_pinch("dev0".to_owned());

    // Past the configured maximum.
    f.niri_state().update_zoom_pinch(&output, 20.);
    assert_eq!(zoom_level(&mut f, &output), 4.);

    // Back below 1 snaps to the exact identity.
    f.niri_state().update_zoom_pinch(&output, 0.1);
    assert_eq!(zoom_level(&mut f, &output), 1.);
    assert_eq!(zoom_target_level(&mut f, &output), 1.);
}

#[test]
fn zoom_pinch_commit_on_end() {
    let mut f = set_up_with_pinch();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri_state().begin_zoom_pinch("dev0".to_owned());
    f.niri_state().update_zoom_pinch(&output, 2.);
    f.niri_state().commit_zoom_pinch();

    assert!(zoom_pinch(&mut f).is_none());
    assert!(!zoom_gesturing(&mut f, &output));
    assert_eq!(zoom_level(&mut f, &output), 2.);
    assert_eq!(zoom_target_level(&mut f, &output), 2.);
}

#[test]
fn zoom_pinch_interrupt_commits_and_swallows() {
    let mut f = set_up_with_pinch();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri_state().begin_zoom_pinch("dev0".to_owned());
    f.niri_state().update_zoom_pinch(&output, 2.);
    f.niri_state().interrupt_zoom_pinch();

    // The current gesture state is committed, the rest of the sequence is
    // swallowed rather than forwarded.
    assert!(matches!(
        zoom_pinch(&mut f),
        Some(ZoomPinchRouting::Swallowing { ref device_id }) if device_id == "dev0"
    ));
    assert!(!zoom_gesturing(&mut f, &output));
    assert_eq!(zoom_level(&mut f, &output), 2.);

    // The physical end clears the routing.
    f.niri_state().commit_zoom_pinch();
    assert!(zoom_pinch(&mut f).is_none());
}

#[test]
fn zoom_pinch_explicit_action_interrupts() {
    let mut f = set_up_with_pinch();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri_state().begin_zoom_pinch("dev0".to_owned());
    f.niri_state().update_zoom_pinch(&output, 2.);

    // An explicit zoom action takes over the transition; the next pinch
    // event commits the gesture and swallows the rest of the sequence.
    f.niri_state().do_action(Action::ZoomIn, false);
    f.niri_state().update_zoom_pinch(&output, 3.);

    assert!(matches!(
        zoom_pinch(&mut f),
        Some(ZoomPinchRouting::Swallowing { .. })
    ));
    assert!(!zoom_gesturing(&mut f, &output));
}

#[test]
fn zoom_pinch_hold_release_interrupts() {
    let mut f = set_up_with_pinch();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    // A hold started before the pinch keeps its snapshot; the pinch drives
    // the temporary hold viewport.
    let trigger = key_trigger(100);
    f.niri_state().begin_zoom_hold(trigger, 2., false);
    f.niri_state().begin_zoom_pinch("dev0".to_owned());
    f.niri_state().update_zoom_pinch(&output, 1.5);
    assert_eq!(zoom_level(&mut f, &output), 3.);

    // Releasing the hold restores the snapshot and replaces the gesture;
    // the next pinch event swallows the rest of the sequence.
    hold_release(&mut f, trigger);
    f.niri_state().update_zoom_pinch(&output, 2.);

    assert!(matches!(
        zoom_pinch(&mut f),
        Some(ZoomPinchRouting::Swallowing { .. })
    ));
    assert!(!zoom_gesturing(&mut f, &output));
    assert_eq!(zoom_level(&mut f, &output), 1.);
}

#[test]
fn zoom_pinch_output_removal_swallows() {
    let mut f = set_up_with_pinch();
    let output1 = f.niri_output(1);
    f.add_output(2, (1920, 720));
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri_state().begin_zoom_pinch("dev0".to_owned());
    f.niri_state().update_zoom_pinch(&output1, 2.);

    // Removing the owner output cannot forward the rest of the sequence:
    // the client never saw the begin.
    f.niri().remove_output(&output1);

    assert!(matches!(
        zoom_pinch(&mut f),
        Some(ZoomPinchRouting::Swallowing { .. })
    ));

    f.niri_state().commit_zoom_pinch();
    assert!(zoom_pinch(&mut f).is_none());
}

#[test]
fn zoom_pinch_device_removal_commits() {
    let mut f = set_up_with_pinch();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri_state().begin_zoom_pinch("dev0".to_owned());
    f.niri_state().update_zoom_pinch(&output, 2.);

    // A different device going away does not disturb the gesture.
    f.niri_state().commit_zoom_pinch_for_device("dev1");
    assert!(matches!(
        zoom_pinch(&mut f),
        Some(ZoomPinchRouting::Active { .. })
    ));
    assert!(zoom_gesturing(&mut f, &output));

    // The owner device going away commits the gesture; no physical end will
    // arrive, so the routing is cleared rather than swallowed.
    f.niri_state().commit_zoom_pinch_for_device("dev0");
    assert!(zoom_pinch(&mut f).is_none());
    assert!(!zoom_gesturing(&mut f, &output));
    assert_eq!(zoom_level(&mut f, &output), 2.);
}

#[test]
fn zoom_pinch_owner_output_fixed_at_begin() {
    let mut f = set_up_with_pinch();
    let output1 = f.niri_output(1);
    f.add_output(2, (1920, 720));
    let output2 = f.niri_output(2);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri_state().begin_zoom_pinch("dev0".to_owned());

    // The pointer moves to another output mid-gesture; updates still drive
    // the owner output.
    f.niri_state().move_cursor(Point::from((2000., 100.)));
    f.niri_state().update_zoom_pinch(&output1, 2.);

    assert_eq!(zoom_level(&mut f, &output1), 2.);
    assert_eq!(zoom_level(&mut f, &output2), 1.);
}

#[test]
fn zoom_pinch_config_reload_keeps_sequence() {
    let mut f = set_up_with_pinch();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri_state().begin_zoom_pinch("dev0".to_owned());
    f.niri_state().update_zoom_pinch(&output, 2.);

    // Disabling pinch mid-gesture does not re-route the claimed sequence;
    // the new setting applies to the next begin.
    reload_with_zoom(&mut f, "");
    assert!(matches!(
        zoom_pinch(&mut f),
        Some(ZoomPinchRouting::Active { .. })
    ));
    assert!(!f.niri_state().can_claim_zoom_pinch(3));

    f.niri_state().update_zoom_pinch(&output, 1.5);
    assert_eq!(zoom_level(&mut f, &output), 1.5);
}

#[test]
fn zoom_pinch_max_decrease_interrupts() {
    let mut f = set_up_with_zoom("zoom { pinch-fingers 3; max-zoom 4; }");
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri_state().begin_zoom_pinch("dev0".to_owned());
    f.niri_state().update_zoom_pinch(&output, 4.);
    assert_eq!(zoom_level(&mut f, &output), 4.);

    // Lowering max-zoom clamps the level immediately, replacing the gesture;
    // the rest of the sequence is swallowed.
    reload_with_zoom(&mut f, "zoom { pinch-fingers 3; max-zoom 2; }");
    assert_eq!(zoom_level(&mut f, &output), 2.);

    f.niri_state().update_zoom_pinch(&output, 1.);
    assert!(matches!(
        zoom_pinch(&mut f),
        Some(ZoomPinchRouting::Swallowing { .. })
    ));
}

#[test]
fn zoom_pinch_deadzone_suppressed() {
    let mut f = set_up_with_pinch();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri_state().begin_zoom_pinch("dev0".to_owned());
    f.niri_state().update_zoom_pinch(&output, 2.);

    let focal = f
        .niri()
        .layout
        .monitor_for_output(&output)
        .unwrap()
        .zoom()
        .focal();

    // Pointer motion does not re-pin the gesture anchor.
    f.niri_state()
        .update_zoom_focal_for_cursor(Point::from((1800., 600.)), None);
    let after = f
        .niri()
        .layout
        .monitor_for_output(&output)
        .unwrap()
        .zoom()
        .focal();
    assert_eq!(after, focal);
}

#[test]
fn zoom_pinch_begin_from_animation() {
    let mut f = set_up_with_config(&format!("{ANIMATED_CONFIG}\nzoom {{ pinch-fingers 3; }}"));
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    // Start an animated zoom and freeze it mid-flight.
    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(2.)), false);
    freeze_clock(&mut f);
    advance_clock(&mut f, 50);
    let displayed = zoom_level(&mut f, &output);
    assert!(displayed > 1. && displayed < 2.);

    // The gesture base is the displayed level, not the animation target.
    f.niri_state().begin_zoom_pinch("dev0".to_owned());
    assert_abs_diff_eq!(zoom_level(&mut f, &output), displayed, epsilon = EPS);
    assert_abs_diff_eq!(zoom_target_level(&mut f, &output), displayed, epsilon = EPS);

    f.niri_state().update_zoom_pinch(&output, 2.);
    assert_abs_diff_eq!(zoom_level(&mut f, &output), displayed * 2., epsilon = EPS);
}

#[test]
fn zoom_pinch_second_device_not_claimed() {
    let mut f = set_up_with_pinch();
    let _output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri_state().begin_zoom_pinch("dev0".to_owned());

    // While a sequence is routed, no second zoom gesture is claimed.
    assert!(!f.niri_state().can_claim_zoom_pinch(3));
}

// --- IPC zoom state snapshot ------------------------------------------------

fn ipc_zoom_state(f: &mut Fixture) -> Vec<niri_ipc::ZoomState> {
    crate::ipc::server::zoom_state(f.niri())
}

fn ipc_zoom_for(f: &mut Fixture, output: &Output) -> niri_ipc::ZoomState {
    let name = output.name();
    ipc_zoom_state(f)
        .into_iter()
        .find(|state| state.output == name)
        .unwrap()
}

#[test]
fn zoom_ipc_identity() {
    let mut f = set_up();
    let output = f.niri_output(1);

    let states = ipc_zoom_state(&mut f);
    assert_eq!(states.len(), 1);

    let state = &states[0];
    assert_eq!(state.output, output.name());
    assert_eq!(state.level, 1.);
    assert_eq!(state.target_level, 1.);
    assert_eq!(state.effective_level, 1.);
    assert_eq!(state.focal, (960., 360.));
    assert!(!state.locked);
}

#[test]
fn zoom_ipc_resting_zoom() {
    let mut f = set_up();
    let output = f.niri_output(1);
    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));

    let state = ipc_zoom_for(&mut f, &output);
    assert_eq!(state.level, 2.);
    assert_eq!(state.target_level, 2.);
    assert_eq!(state.effective_level, 2.);
    assert!(!state.locked);
}

#[test]
fn zoom_ipc_mid_animation() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(4.)), false);
    advance_clock(&mut f, 50);

    let state = ipc_zoom_for(&mut f, &output);
    assert!(
        state.level > 1. && state.level < 4.,
        "intermediate level: {}",
        state.level
    );
    assert_eq!(state.target_level, 4.);
    // With the overview closed the effective level is the displayed level.
    assert_eq!(state.effective_level, state.level);
}

#[test]
fn zoom_ipc_gesturing() {
    let mut f = set_up_with_pinch();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((150., 100.)));

    f.niri_state().begin_zoom_pinch("dev0".to_owned());
    f.niri_state().update_zoom_pinch(&output, 2.);

    let state = ipc_zoom_for(&mut f, &output);
    assert_eq!(state.level, 2.);
    assert_eq!(state.target_level, 2.);
    assert_eq!(state.effective_level, 2.);
}

#[test]
fn zoom_ipc_locked() {
    let mut f = set_up();
    let output = f.niri_output(1);
    set_zoom(&mut f, &output, 2., Point::from((960., 360.)));

    f.niri()
        .layout
        .monitor_for_output_mut(&output)
        .unwrap()
        .zoom_mut()
        .set_locked(true);

    let state = ipc_zoom_for(&mut f, &output);
    assert!(state.locked);
    assert_eq!(state.level, 2.);
    assert_eq!(state.target_level, 2.);
    assert_eq!(state.effective_level, 2.);
}

#[test]
fn zoom_ipc_focal_matches_domain() {
    let mut f = set_up();
    let output = f.niri_output(1);
    set_zoom(&mut f, &output, 2., Point::from((100., 200.)));

    let state = ipc_zoom_for(&mut f, &output);
    let domain_focal = zoom_focal(&mut f, &output);
    assert_eq!(state.focal, (domain_focal.x, domain_focal.y));
}

#[test]
fn zoom_ipc_multi_output() {
    let mut f = set_up();
    let output1 = f.niri_output(1);
    f.add_output(2, (1920, 720));
    let output2 = f.niri_output(2);

    set_zoom(&mut f, &output1, 2., Point::from((960., 360.)));
    set_zoom(&mut f, &output2, 4., Point::from((960., 360.)));
    f.niri()
        .layout
        .monitor_for_output_mut(&output2)
        .unwrap()
        .zoom_mut()
        .set_locked(true);

    let states = ipc_zoom_state(&mut f);
    assert_eq!(states.len(), 2);

    let state1 = states.iter().find(|s| s.output == output1.name()).unwrap();
    assert_eq!(state1.level, 2.);
    assert!(!state1.locked);

    let state2 = states.iter().find(|s| s.output == output2.name()).unwrap();
    assert_eq!(state2.level, 4.);
    assert!(state2.locked);
}

#[test]
fn zoom_ipc_output_removal() {
    let mut f = set_up();
    let output1 = f.niri_output(1);
    f.add_output(2, (1920, 720));
    let output2 = f.niri_output(2);
    set_zoom(&mut f, &output2, 4., Point::from((960., 360.)));

    f.niri().remove_output(&output2);

    let states = ipc_zoom_state(&mut f);
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].output, output1.name());
}

#[test]
fn zoom_ipc_overview_suppression() {
    let mut f = set_up();
    let output = f.niri_output(1);
    set_zoom(&mut f, &output, 4., Point::from((960., 360.)));

    // Partial overview: the effective level moves towards 1 while the stored
    // state is untouched.
    set_overview_progress(&mut f, &output, Some(0.5));
    let state = ipc_zoom_for(&mut f, &output);
    assert_eq!(state.level, 4.);
    assert_eq!(state.target_level, 4.);
    assert_abs_diff_eq!(state.effective_level, 2., epsilon = EPS);

    // Fully open overview: the desktop scene presents at the identity.
    set_overview_open(&mut f, &output, true);
    let state = ipc_zoom_for(&mut f, &output);
    assert_eq!(state.level, 4.);
    assert_eq!(state.target_level, 4.);
    assert_eq!(state.effective_level, 1.);
    assert!(!state.locked);
}

#[test]
fn zoom_ipc_effective_level_is_not_presentation_transform() {
    // effective_level describes the desktop scene after Overview suppression.
    // The session lock replaces the whole presentation with the identity, but
    // that is a separate transform: a stored zoom still reports its level.
    let mut f = set_up();
    let output = f.niri_output(1);
    set_zoom(&mut f, &output, 4., Point::from((960., 360.)));

    let state = ipc_zoom_for(&mut f, &output);
    assert_eq!(state.level, 4.);
    assert_eq!(state.effective_level, 4.);

    // The session-lock presentation transform is identity regardless; the IPC
    // snapshot deliberately does not model it.
    let effective = effective_zoom_transform(&mut f, &output);
    assert_eq!(
        crate::niri::Niri::pointer_transform_for_presentation(true, effective),
        crate::utils::view::ViewportTransform::identity()
    );
}

#[test]
fn zoom_ipc_query_is_side_effect_free() {
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(4.)), false);
    advance_clock(&mut f, 50);

    let first = ipc_zoom_state(&mut f);
    let second = ipc_zoom_state(&mut f);
    assert_eq!(first, second);

    // The query did not finish the transition or change the target.
    assert!(zoom_is_animating(&mut f, &output));
    assert_eq!(zoom_target_level(&mut f, &output), 4.);
}

// --- continuous deadzone follow ---

#[test]
fn zoom_follow_static_pointer_moves_camera() {
    // Regression: a pointer resting outside the deadzone must keep moving the
    // camera until it reaches the deadzone boundary, without new pointer
    // input. On the old immediate-tracking semantics the focal point jumped
    // once on the warp and then froze.
    let mut f = set_up_animated();
    let output = f.niri_output(1);
    f.niri_state().move_cursor(Point::from((960., 360.)));
    freeze_clock(&mut f);

    f.niri_state()
        .do_action(Action::SetZoomLevel(FloatOrInt(2.)), false);
    advance_clock(&mut f, 5000);
    assert_eq!(zoom_level(&mut f, &output), 2.);

    // Warp to the content corner: the displayed pointer lands outside the
    // deadzone and the physical pointer does not move again.
    f.niri_state().move_cursor(Point::from((0., 0.)));
    let focal = zoom_focal(&mut f, &output);

    // Without further pointer input the camera must keep moving until the
    // displayed pointer reaches the deadzone boundary.
    advance_clock(&mut f, 50);
    assert_ne!(
        zoom_focal(&mut f, &output),
        focal,
        "a static pointer outside the deadzone must keep moving the camera"
    );

    advance_clock(&mut f, 5000);
    assert!(!zoom_is_animating(&mut f, &output));
    // The viewport clamp stops the follow at the output edge: the pointer
    // stays outside the deadzone and no further correction is possible.
    assert_eq!(zoom_focal(&mut f, &output), Point::from((0., 0.)));
    assert_eq!(displayed_pointer_location(&mut f), Point::from((0., 0.)));
}
