use niri_ipc::{OutputRegion, RegionFrameCommand, RegionFrameSpec, RegionGeometry, Response};
use smithay::backend::allocator::Fourcc;
use smithay::backend::input::InputTime;
use smithay::utils::{Point, Scale, Transform, SERIAL_COUNTER};
use wayland_client::protocol::wl_pointer;

use super::Fixture;
use crate::render_helpers::{render_to_vec, RenderCtx, RenderTarget};

fn setup() -> Fixture {
    let config = niri_config::Config::parse_mem(
        r#"
        animations { off; }
        hotkey-overlay { skip-at-startup; }
        output "headless-1" { scale 1.5; position x=-640 y=0; }
    "#,
    )
    .unwrap();
    let mut f = Fixture::with_config(config);
    f.niri_state().backend.headless().add_renderer().unwrap();
    f.add_output(1, (960, 720));
    f
}

fn spec() -> RegionFrameSpec {
    RegionFrameSpec {
        region: OutputRegion {
            output: "headless-1".into(),
            geometry: RegionGeometry {
                x: 100,
                y: 80,
                width: 200,
                height: 120,
            },
        },
        color: "#ff00ff".into(),
    }
}

fn frame_pixels(
    f: &mut Fixture,
    output: &smithay::output::Output,
    target: Option<RenderTarget>,
) -> Vec<u8> {
    let state = f.niri_state();
    state.niri.update_render_elements(Some(output));
    let size = output
        .current_transform()
        .transform_size(output.current_mode().unwrap().size);
    let scale = Scale::from(output.current_scale().fractional_scale());
    state
        .backend
        .headless()
        .with_primary_renderer(|renderer| {
            let ctx = RenderCtx {
                renderer,
                target: target.unwrap_or(RenderTarget::Output),
                xray: None,
            };
            let elements = match target {
                Some(_) => state.niri.render_to_vec(ctx, output, false),
                None => state.niri.render_for_output(ctx, output),
            };
            render_to_vec(
                renderer,
                size,
                scale,
                Transform::Normal,
                Fourcc::Abgr8888,
                elements.iter().rev(),
            )
            .unwrap()
        })
        .unwrap()
}

fn magenta_positions(pixels: &[u8]) -> Vec<usize> {
    pixels
        .chunks_exact(4)
        .enumerate()
        .filter_map(|(i, p)| (p == [255, 0, 255, 255]).then_some(i))
        .collect()
}

#[test]
fn region_frame_fixed_across_zoom_overview_workspace_and_excluded_from_capture() {
    let mut f = setup();
    let output = f.niri_output(1);
    f.niri_state()
        .region_frame_command(RegionFrameCommand::Set(spec()))
        .unwrap();
    let initial = magenta_positions(&frame_pixels(&mut f, &output, None));
    // Logical region 100,80 200x120 at 1.5x scale; inward 2px logical border.
    let expected: Vec<_> = (120..300)
        .flat_map(|y| {
            (150..450).filter_map(move |x| {
                (!(123..297).contains(&y) || !(153..447).contains(&x)).then_some(y * 960 + x)
            })
        })
        .collect();
    assert_eq!(initial, expected);
    f.niri()
        .layout
        .monitor_for_output_mut(&output)
        .unwrap()
        .zoom_mut()
        .set_level_immediate(2.5, Point::from((200., 120.)));
    assert_eq!(
        magenta_positions(&frame_pixels(&mut f, &output, None)),
        expected
    );
    f.niri_state().toggle_overview();
    f.niri_complete_animations();
    assert_eq!(
        magenta_positions(&frame_pixels(&mut f, &output, None)),
        expected
    );
    f.niri_state().toggle_overview();
    f.niri_state()
        .do_action(niri_config::Action::FocusWorkspaceDown, false);
    f.niri_complete_animations();
    assert_eq!(
        magenta_positions(&frame_pixels(&mut f, &output, None)),
        expected
    );
    for target in [
        RenderTarget::Output,
        RenderTarget::ScreenCapture,
        RenderTarget::Screencast,
    ] {
        assert!(magenta_positions(&frame_pixels(&mut f, &output, Some(target))).is_empty());
    }
    f.add_output(2, (640, 480));
    let second = f.niri_output(2);
    assert!(magenta_positions(&frame_pixels(&mut f, &second, None)).is_empty());
    f.niri().global_space.map_output(&output, (1000, 100));
    assert_eq!(
        magenta_positions(&frame_pixels(&mut f, &output, None)),
        expected
    );
    // The screenshot UI samples Output textures, not ScreenCapture textures.
    f.niri_state().open_screenshot_ui(false, None);
    let state = f.niri_state();
    let (_, screenshot) = state
        .backend
        .headless()
        .with_primary_renderer(|renderer| state.niri.screenshot_ui.capture(renderer).unwrap())
        .unwrap();
    assert!(magenta_positions(&screenshot).is_empty());
}

#[test]
fn region_frame_lifecycle_preserves_existing_frame_on_invalid_update() {
    let mut f = setup();
    let output = f.niri_output(1);
    f.niri_state()
        .region_frame_command(RegionFrameCommand::Set(spec()))
        .unwrap();
    assert!(f
        .niri_state()
        .region_frame_command(RegionFrameCommand::SetColor("not-a-color".into()))
        .is_err());
    let mut invalid = spec();
    invalid.region.geometry.width = u32::MAX;
    assert!(f
        .niri_state()
        .region_frame_command(RegionFrameCommand::Set(invalid))
        .is_err());
    let Response::RegionFrame(Some(current)) = f
        .niri_state()
        .region_frame_command(RegionFrameCommand::Get)
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(current, spec());
    f.niri_state()
        .region_frame_command(RegionFrameCommand::SetColor("red".into()))
        .unwrap();
    let Response::RegionFrame(Some(current)) = f
        .niri_state()
        .region_frame_command(RegionFrameCommand::Get)
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(current.region, spec().region);
    assert_eq!(current.color, "red");
    f.niri().remove_output(&output);
    assert!(matches!(
        f.niri_state()
            .region_frame_command(RegionFrameCommand::Get)
            .unwrap(),
        Response::RegionFrame(None)
    ));
    f.niri_state()
        .region_frame_command(RegionFrameCommand::Clear)
        .unwrap();
    assert!(f
        .niri_state()
        .region_frame_command(RegionFrameCommand::SetColor("red".into()))
        .is_err());
}

#[test]
fn region_selection_uses_displayed_coordinates_and_preserves_frame() {
    let mut f = setup();
    let output = f.niri_output(1);
    f.niri_state()
        .region_frame_command(RegionFrameCommand::Set(spec()))
        .unwrap();
    f.niri()
        .layout
        .monitor_for_output_mut(&output)
        .unwrap()
        .zoom_mut()
        .set_level_immediate(2., Point::from((200., 150.)));
    let id = f.add_client();
    let client = f.client(id);
    let pointer = client
        .state
        .virtual_pointer_manager
        .as_ref()
        .unwrap()
        .create_virtual_pointer(None, &client.qh, ());
    let (tx, rx) = async_channel::bounded(1);
    f.niri_state().start_region_selection(tx).unwrap();
    let (busy_tx, _) = async_channel::bounded(1);
    assert!(f.niri_state().start_region_selection(busy_tx).is_err());
    pointer.motion_absolute(0, 100, 80, 640, 480);
    pointer.button(0, 0x110, wl_pointer::ButtonState::Pressed);
    pointer.frame();
    f.roundtrip(id);
    pointer.motion_absolute(1, 300, 200, 640, 480);
    pointer.button(1, 0x110, wl_pointer::ButtonState::Released);
    pointer.frame();
    f.roundtrip(id);
    let result = rx.try_recv().unwrap().unwrap();
    assert_eq!(result.region, spec().region);
    assert_eq!(
        result.global_geometry,
        RegionGeometry {
            x: -540,
            y: 80,
            width: 200,
            height: 120
        }
    );
    assert!(!f.niri().seat.get_pointer().unwrap().is_grabbed());
    let Response::RegionFrame(Some(frame)) = f
        .niri_state()
        .region_frame_command(RegionFrameCommand::Get)
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(frame, spec());
}

#[test]
fn region_selection_cancel_and_output_removal_resolve_pending_request() {
    let mut f = setup();
    let (tx, rx) = async_channel::bounded(1);
    f.niri_state().start_region_selection(tx).unwrap();
    let pointer = f.niri().seat.get_pointer().unwrap();
    pointer.unset_grab(
        f.niri_state(),
        SERIAL_COUNTER.next_serial(),
        InputTime::now(),
    );
    assert!(rx.try_recv().unwrap().is_none());
    assert!(f.niri().region_selection.is_none());
    let (tx, rx) = async_channel::bounded(1);
    f.niri_state().start_region_selection(tx).unwrap();
    let output = f.niri_output(1);
    f.niri().remove_output(&output);
    f.state.server.dispatch();
    assert!(rx.try_recv().unwrap().is_none());
    assert!(!f.niri().seat.get_pointer().unwrap().is_grabbed());
}

#[test]
fn region_selection_retries_empty_click_and_clamps_to_initial_output() {
    let mut f = setup();
    f.add_output(2, (640, 480));
    let id = f.add_client();
    let client = f.client(id);
    let pointer = client
        .state
        .virtual_pointer_manager
        .as_ref()
        .unwrap()
        .create_virtual_pointer(None, &client.qh, ());
    let (tx, rx) = async_channel::bounded(1);
    f.niri_state().start_region_selection(tx).unwrap();
    pointer.motion_absolute(0, 100, 80, 1280, 480);
    pointer.button(0, 0x110, wl_pointer::ButtonState::Pressed);
    pointer.button(0, 0x110, wl_pointer::ButtonState::Released);
    pointer.frame();
    f.roundtrip(id);
    assert!(matches!(
        rx.try_recv(),
        Err(async_channel::TryRecvError::Empty)
    ));
    pointer.button(0, 0x110, wl_pointer::ButtonState::Pressed);
    pointer.motion_absolute(1, 800, 200, 1280, 480);
    pointer.button(1, 0x110, wl_pointer::ButtonState::Released);
    pointer.frame();
    f.roundtrip(id);
    let region = rx.try_recv().unwrap().unwrap().region;
    assert_eq!(region.output, "headless-1");
    assert_eq!(
        region.geometry,
        RegionGeometry {
            x: 100,
            y: 80,
            width: 540,
            height: 120
        }
    );
}
