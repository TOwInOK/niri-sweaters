use async_channel::Sender;
use niri_ipc::{
    OutputRegion, RegionFrameCommand, RegionFrameSpec, RegionGeometry, Response, SelectedRegion,
};
use smithay::backend::input::{ButtonState, InputTime};
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, CursorIcon, CursorImageStatus, Focus, GestureHoldBeginEvent,
    GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent,
    GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent, GrabStartData,
    MotionEvent, PointerGrab, PointerInnerHandle, RelativeMotionEvent,
};
use smithay::input::SeatHandler;
use smithay::output::Output;
use smithay::utils::{Logical, Point, Size, SERIAL_COUNTER};

use crate::niri::{Niri, OutputRenderElements, State};
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::RenderCtx;
use crate::ui::region::RegionFrame;

pub(crate) struct RegionSelection {
    phase: SelectionPhase,
    reply: Sender<Option<SelectedRegion>>,
}

enum SelectionPhase {
    AwaitingPress,
    Dragging(Box<SelectionDrag>),
}

struct SelectionDrag {
    output: Output,
    size: Size<i32, Logical>,
    anchor: Point<f64, Logical>,
    current: Point<f64, Logical>,
    preview: Option<RegionFrame>,
}

impl RegionSelection {
    fn output(&self) -> Option<&Output> {
        match &self.phase {
            SelectionPhase::AwaitingPress => None,
            SelectionPhase::Dragging(drag) => Some(&drag.output),
        }
    }
}

impl Drop for RegionSelection {
    fn drop(&mut self) {
        // Every cancellation path, including compositor shutdown, resolves the IPC request.
        let _ = self.reply.try_send(None);
    }
}

impl State {
    pub(crate) fn start_region_selection(
        &mut self,
        reply: Sender<Option<SelectedRegion>>,
    ) -> Result<(), String> {
        let pointer = self.niri.seat.get_pointer().unwrap();
        if self.niri.is_locked() {
            return Err("cannot select a region while locked".into());
        }
        if self.niri.region_selection.is_some()
            || pointer.is_grabbed()
            || self.niri.screenshot_ui.is_open()
            || self.niri.window_mru_ui.is_active()
            || self.niri.exit_confirm_dialog.is_open()
            || self.niri.popup_grab.is_some()
        {
            return Err("another interactive operation is active".into());
        }
        if self.niri.global_space.outputs().next().is_none() {
            return Err("no outputs available".into());
        }
        let start_data = GrabStartData {
            focus: None,
            button: 0,
            location: pointer.current_location(),
        };
        self.niri.region_selection = Some(RegionSelection {
            phase: SelectionPhase::AwaitingPress,
            reply,
        });
        pointer.set_grab(
            self,
            RegionSelectionGrab { start_data },
            SERIAL_COUNTER.next_serial(),
            Focus::Clear,
        );
        self.niri
            .cursor_manager
            .set_cursor_image(CursorImageStatus::Named(CursorIcon::Crosshair));
        self.niri.queue_redraw_all();
        Ok(())
    }

    pub(crate) fn region_frame_command(
        &mut self,
        command: RegionFrameCommand,
    ) -> Result<Response, String> {
        match command {
            RegionFrameCommand::Get => {
                return Ok(Response::RegionFrame(
                    self.niri.region_frame.as_ref().map(RegionFrame::spec),
                ))
            }
            RegionFrameCommand::Set(spec) => {
                let output = self
                    .niri
                    .global_space
                    .outputs()
                    .find(|output| output.name() == spec.region.output)
                    .cloned()
                    .ok_or_else(|| "output not found".to_string())?;
                let frame = RegionFrame::new(output, spec)?;
                self.niri.region_frame = Some(frame);
            }
            RegionFrameCommand::SetColor(color) => {
                self.niri
                    .region_frame
                    .as_mut()
                    .ok_or_else(|| "no region frame is active".to_string())?
                    .set_color(color)?;
            }
            RegionFrameCommand::Clear => {
                self.niri.region_frame = None;
            }
        }
        self.niri.queue_redraw_all();
        Ok(Response::Handled)
    }
}

impl Niri {
    pub(crate) fn cancel_region_selection(&mut self) {
        if self.region_selection.take().is_none() {
            return;
        }
        self.cursor_manager
            .set_cursor_image(CursorImageStatus::default_named());
        self.queue_redraw_all();
        // Niri alone cannot unset a Smithay grab: defer until State is available.
        self.event_loop.insert_idle(|state| {
            let pointer = state.niri.seat.get_pointer().unwrap();
            if pointer
                .with_grab(|_, grab| grab.as_any().is::<RegionSelectionGrab>())
                .unwrap_or(false)
            {
                pointer.unset_grab(state, SERIAL_COUNTER.next_serial(), InputTime::now());
            }
        });
    }

    pub(crate) fn region_output_resized(&mut self, output: &Output) {
        if let Some(RegionSelection {
            phase: SelectionPhase::Dragging(drag),
            ..
        }) = &self.region_selection
        {
            if &drag.output == output
                && drag.size != crate::utils::output_size(output).to_i32_ceil()
            {
                self.cancel_region_selection();
            }
        }
    }

    pub(crate) fn region_output_removed(&mut self, output: &Output) {
        if self
            .region_frame
            .as_ref()
            .is_some_and(|frame| frame.output() == output)
        {
            self.region_frame = None;
        }
        if self
            .region_selection
            .as_ref()
            .is_some_and(|selection| selection.output().is_none_or(|selected| selected == output))
        {
            self.cancel_region_selection();
        }
    }

    /// Only real display backends use this entry point. Capture paths use render_to_vec.
    pub(crate) fn render_for_output<R: NiriRenderer>(
        &self,
        ctx: RenderCtx<R>,
        output: &Output,
    ) -> Vec<OutputRenderElements<R>> {
        let mut elements = Vec::new();
        if !self.is_locked() {
            let mut push =
                |element: crate::render_helpers::solid_color::SolidColorRenderElement| {
                    elements.push(element.into())
                };
            if let Some(RegionSelection {
                phase: SelectionPhase::Dragging(drag),
                ..
            }) = &self.region_selection
            {
                if let Some(preview) = &drag.preview {
                    preview.render(output, &mut push);
                }
            }
            if let Some(frame) = &self.region_frame {
                frame.render(output, &mut push);
            }
        }
        self.render(ctx, output, true, &mut |element| elements.push(element));
        elements
    }

    fn region_pointer_down(&mut self, displayed: Point<f64, Logical>) {
        let Some((output, local)) = self.output_under(displayed) else {
            return;
        };
        let output = output.clone();
        if let Some(selection) = &mut self.region_selection {
            if matches!(selection.phase, SelectionPhase::AwaitingPress) {
                selection.phase = SelectionPhase::Dragging(Box::new(SelectionDrag {
                    size: crate::utils::output_size(&output).to_i32_ceil(),
                    output,
                    anchor: local,
                    current: local,
                    preview: None,
                }));
            }
        }
    }

    fn region_pointer_motion(&mut self, displayed: Point<f64, Logical>) {
        let Some(selection) = &mut self.region_selection else {
            return;
        };
        let SelectionPhase::Dragging(drag) = &mut selection.phase else {
            return;
        };
        let Some(bounds) = self.global_space.output_geometry(&drag.output) else {
            return;
        };
        let local = displayed - bounds.loc.to_f64();
        drag.current = Point::from((
            local.x.clamp(0., f64::from(bounds.size.w)),
            local.y.clamp(0., f64::from(bounds.size.h)),
        ));
        if let Some(geometry) = region_geometry(drag.anchor, drag.current) {
            if let Some(preview) = &mut drag.preview {
                let _ = preview.set_geometry(geometry);
            } else {
                drag.preview = RegionFrame::new(
                    drag.output.clone(),
                    RegionFrameSpec {
                        region: OutputRegion {
                            output: drag.output.name(),
                            geometry,
                        },
                        color: "white".into(),
                    },
                )
                .ok();
            }
        } else {
            drag.preview = None;
        }
        self.queue_redraw_all();
    }

    fn finish_region_selection(&mut self) -> bool {
        let Some(selection) = &mut self.region_selection else {
            return true;
        };
        let SelectionPhase::Dragging(drag) = &selection.phase else {
            return false;
        };
        let Some(geometry) = region_geometry(drag.anchor, drag.current) else {
            selection.phase = SelectionPhase::AwaitingPress;
            self.queue_redraw_all();
            return false;
        };
        let Some(bounds) = self.global_space.output_geometry(&drag.output) else {
            self.cancel_region_selection();
            return true;
        };
        let global = geometry
            .x
            .checked_add(bounds.loc.x)
            .zip(geometry.y.checked_add(bounds.loc.y));
        let result = global.map(|(x, y)| SelectedRegion {
            region: OutputRegion {
                output: drag.output.name(),
                geometry,
            },
            global_geometry: RegionGeometry { x, y, ..geometry },
        });
        let _ = selection.reply.try_send(result);
        selection.reply.close();
        self.region_selection = None;
        self.queue_redraw_all();
        true
    }
}

fn region_geometry(a: Point<f64, Logical>, b: Point<f64, Logical>) -> Option<RegionGeometry> {
    if a.x == b.x || a.y == b.y {
        return None;
    }
    let x = a.x.min(b.x).floor() as i32;
    let y = a.y.min(b.y).floor() as i32;
    let right = a.x.max(b.x).ceil() as i32;
    let bottom = a.y.max(b.y).ceil() as i32;
    Some(RegionGeometry {
        x,
        y,
        width: u32::try_from(right.checked_sub(x)?).ok()?,
        height: u32::try_from(bottom.checked_sub(y)?).ok()?,
    })
}

pub(crate) struct RegionSelectionGrab {
    start_data: GrabStartData<State>,
}

impl PointerGrab<State> for RegionSelectionGrab {
    fn motion(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(<State as SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        let displayed = data.niri.display_position_for_content(event.location);
        data.niri.region_pointer_motion(displayed);
        handle.motion(data, None, event);
    }
    fn relative_motion(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(<State as SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, None, event);
    }
    fn button(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &ButtonEvent,
    ) {
        if event.button != 0x110 {
            return;
        }
        let displayed = data
            .niri
            .display_position_for_content(handle.current_location());
        match event.state {
            ButtonState::Pressed => {
                data.niri.region_pointer_down(displayed);
            }
            ButtonState::Released => {
                data.niri.region_pointer_motion(displayed);
                if data.niri.finish_region_selection() {
                    handle.unset_grab(self, data, event.serial, event.time, true);
                }
            }
        }
    }
    fn axis(
        &mut self,
        _data: &mut State,
        _handle: &mut PointerInnerHandle<'_, State>,
        _details: AxisFrame,
    ) {
    }
    fn frame(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>) {
        handle.frame(data);
    }
    fn gesture_swipe_begin(
        &mut self,
        _data: &mut State,
        _handle: &mut PointerInnerHandle<'_, State>,
        _event: &GestureSwipeBeginEvent,
    ) {
    }
    fn gesture_swipe_update(
        &mut self,
        _data: &mut State,
        _handle: &mut PointerInnerHandle<'_, State>,
        _event: &GestureSwipeUpdateEvent,
    ) {
    }
    fn gesture_swipe_end(
        &mut self,
        _data: &mut State,
        _handle: &mut PointerInnerHandle<'_, State>,
        _event: &GestureSwipeEndEvent,
    ) {
    }
    fn gesture_pinch_begin(
        &mut self,
        _data: &mut State,
        _handle: &mut PointerInnerHandle<'_, State>,
        _event: &GesturePinchBeginEvent,
    ) {
    }
    fn gesture_pinch_update(
        &mut self,
        _data: &mut State,
        _handle: &mut PointerInnerHandle<'_, State>,
        _event: &GesturePinchUpdateEvent,
    ) {
    }
    fn gesture_pinch_end(
        &mut self,
        _data: &mut State,
        _handle: &mut PointerInnerHandle<'_, State>,
        _event: &GesturePinchEndEvent,
    ) {
    }
    fn gesture_hold_begin(
        &mut self,
        _data: &mut State,
        _handle: &mut PointerInnerHandle<'_, State>,
        _event: &GestureHoldBeginEvent,
    ) {
    }
    fn gesture_hold_end(
        &mut self,
        _data: &mut State,
        _handle: &mut PointerInnerHandle<'_, State>,
        _event: &GestureHoldEndEvent,
    ) {
    }
    fn start_data(&self) -> &GrabStartData<State> {
        &self.start_data
    }
    fn unset(&mut self, data: &mut State) {
        data.niri.region_selection = None;
        data.niri
            .cursor_manager
            .set_cursor_image(CursorImageStatus::default_named());
        data.niri.queue_redraw_all();
    }
}
