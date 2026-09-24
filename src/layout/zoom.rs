//! Per-output desktop zoom state.
//!
//! Holds the canonical zoom state for a single output and implements the
//! viewport/deadzone math on top of [`ViewportTransform`]. All coordinates are
//! output-local logical. Rendering and input integration live elsewhere.
//!
//! The state is an explicit finite-state machine: [`OutputZoomState`] is an
//! enum whose variants are the only legal combinations of resting state,
//! autonomous level transitions, direct-manipulation gestures, deadzone
//! follows and the zoom lock. There is no authoritative target level outside
//! an in-progress operation: the resting level lives in [`ZoomView`], and the
//! level an operation animates towards lives inside that operation's payload.

use std::mem;
use std::time::Duration;

use smithay::utils::{Logical, Point, Rectangle, Size};
use tracing::trace;

use crate::animation::{Animation, Clock};
use crate::utils::view::ViewportTransform;

/// Desktop zoom state of a single output.
///
/// The variants are the complete lifecycle: a resting state owns only a
/// [`ZoomView`]; autonomous operations ([`ZoomCommandState`],
/// [`RestorePayload`]) own the level animation; a gesture owns the level
/// directly; a follow owns only the focal point. `Locked*` variants are the
/// locked counterparts of the unlocked lifecycle.
///
/// While a transition is active, [`level()`](Self::level) and
/// [`focal()`](Self::focal) report the animated presentation values; the
/// committed `view` fields are only updated when the transition completes.
///
/// The payload types are public so the state can be inspected, but their
/// fields are private: only this module can construct or mutate them, so
/// every state change goes through a domain operation.
#[derive(Debug, Clone)]
pub enum OutputZoomState {
    /// Resting, unlocked: the level is at rest and no follow is active.
    Idle(ZoomView),
    /// A deadzone-driven camera follow at a resting zoom level.
    ///
    /// The level is already at its target; only the focal point moves,
    /// driven by the shared distance-driven follow physics towards
    /// `follow.to_focal`.
    Follow { view: ZoomView, follow: FollowState },
    /// An autonomous level transition owns the viewport.
    Zooming {
        view: ZoomView,
        zooming: ZoomingState,
    },
    /// A level command and a deadzone drift of its anchor run together.
    ///
    /// The drift is the follow's share of the level transition: the camera
    /// starts following the cursor during the zoom animation instead of
    /// after it. When the command completes first, the drift continues
    /// seamlessly as a resting [`Self::Follow`].
    ZoomingFollow {
        view: ZoomView,
        zooming: ZoomCommandState,
        follow: CombinedFollow,
    },
    /// A direct-manipulation gesture owns the level.
    Gesture {
        view: ZoomView,
        gesture: GesturePayload,
    },
    /// Resting, locked: the camera does not track the pointer.
    Locked(ZoomView),
    /// A locked autonomous level transition owns the viewport.
    LockedZooming {
        view: ZoomView,
        zooming: LockedZoomingState,
    },
    /// A locked direct-manipulation gesture owns the level.
    LockedGesture {
        view: ZoomView,
        gesture: LockedGesturePayload,
    },
}

/// The resting zoom view: committed level, focal point and output size.
///
/// `level` is the committed zoom level. `focal` is the fixed point of the
/// viewport transform; it is kept within the output so that the logical
/// viewport never leaves the output bounds. `view_size` is the output-local
/// logical size of the output, needed to clamp derived focal points in the
/// accessors.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ZoomView {
    level: f64,
    focal: Point<f64, Logical>,
    view_size: Size<f64, Logical>,
}

/// An autonomous level transition of an unlocked output.
#[derive(Debug, Clone)]
pub enum ZoomingState {
    /// A user zoom command: animates `log2(level)` towards `target`.
    Command(ZoomCommandState),
    /// A viewport restore driven by an [`Animation`] over progress `0 → 1`.
    Restore(RestorePayload),
}

/// An autonomous level transition of a locked output.
///
/// A locked output has no restores: a restore requested while locked becomes
/// a regular command, and locking during a restore converts it in place.
#[derive(Debug, Clone)]
pub enum LockedZoomingState {
    /// A user zoom command: animates `log2(level)` towards `target`.
    Command(LockedCommandState),
}

/// A zoom command of an unlocked output.
///
/// `In`, `Out` and `Value` record which operation created the command
/// (`zoom-in`, `zoom-out`, `set-target-level`); they share the anchored
/// payload and identical physics. `ToIdentity` is the `target == 1` special
/// case: the anchored focal solve degenerates as the level approaches 1 (it
/// divides by `level - 1`), so the command keeps the focal point fixed and
/// the zoom-out pivots around its displayed position instead.
#[derive(Debug, Clone)]
pub enum ZoomCommandState {
    /// A `zoom-in` command towards `target`.
    In { target: f64, payload: ZoomCommand },
    /// A `zoom-out` command towards `target`.
    Out { target: f64, payload: ZoomCommand },
    /// An absolute `set-target-level` command towards `target`.
    Value { target: f64, payload: ZoomCommand },
    /// A command towards the identity transform (`target == 1`).
    ToIdentity { payload: IdentityCommand },
}

/// A zoom command of a locked output.
///
/// The anchor is fixed throughout the command: the pointer for incremental
/// commands and locking presets, or the viewport center for absolute commands.
/// `ToIdentity` keeps the focal point fixed like the unlocked variant.
#[derive(Debug, Clone)]
pub enum LockedCommandState {
    /// A `zoom-in` command towards `target`.
    In {
        target: f64,
        payload: LockedZoomCommand,
    },
    /// A `zoom-out` command towards `target`.
    Out {
        target: f64,
        payload: LockedZoomCommand,
    },
    /// An absolute `set-target-level` command towards `target`.
    Value {
        target: f64,
        payload: LockedZoomCommand,
    },
    /// A command towards the identity transform (`target == 1`).
    ToIdentity { payload: LockedIdentityCommand },
}

/// Payload of an anchored unlocked zoom command.
///
/// The level animates in `log2` space so that equal multiplicative steps
/// (1x→2x and 2x→4x) cover equal animation distance. The anchor keeps the
/// `content` point at the `display` position while the level animates,
/// subject to the output bounds clamp.
#[derive(Debug, Clone)]
pub struct ZoomCommand {
    animation: Animation,
    anchor: FlexibleAnchor,
}

/// Payload of a zoom command towards the identity transform.
///
/// The focal point stays fixed while the level animates to exactly 1: the
/// anchored solve is degenerate at the identity transform, so the zoom-out
/// pivots around the focal point's displayed position and lands
/// continuously.
#[derive(Debug, Clone)]
pub struct IdentityCommand {
    animation: Animation,
    focal: Point<f64, Logical>,
}

/// Payload of an anchored locked zoom command.
#[derive(Debug, Clone)]
pub struct LockedZoomCommand {
    animation: Animation,
    anchor: LockedAnchor,
}

/// Payload of a locked zoom command towards the identity transform.
#[derive(Debug, Clone)]
pub struct LockedIdentityCommand {
    animation: Animation,
    focal: Point<f64, Logical>,
}

/// The anchor of an unlocked zoom command.
///
/// Both points are output-local logical coordinates snapshotted at
/// (re)target time: `display` is where `content` was shown when the
/// transition started. While a [`CombinedFollow`] drift is active, `display`
/// is mutated in place by the distance-driven step.
#[derive(Debug, Clone, Copy)]
pub struct FlexibleAnchor {
    content: Point<f64, Logical>,
    display: Point<f64, Logical>,
}

/// The fixed anchor of a locked zoom command.
///
/// `display` is the output-local position snapshotted at command creation and
/// `content` is the content point displayed there. Neither follows the pointer.
#[derive(Debug, Clone, Copy)]
pub struct LockedAnchor {
    content: Point<f64, Logical>,
    display: Point<f64, Logical>,
}

/// A viewport restore driven by an [`Animation`] over progress `0 → 1`.
///
/// Used to return from a `zoom hold=true` override: unlike a command, which keeps
/// an action anchor in place, a restore moves the whole viewport back to a
/// saved state. The level still animates in `log2` space between
/// `from_level` and `to_level`, while the focal point interpolates linearly
/// between `from_focal` and `to_focal`. When restoring to 1x the focal point
/// stays at `from_focal` instead: the destination focal point is degenerate
/// at the identity transform, so moving it early would only pan the
/// viewport.
#[derive(Debug, Clone)]
pub struct RestorePayload {
    animation: Animation,
    from_level: f64,
    to_level: f64,
    from_focal: Point<f64, Logical>,
    to_focal: Point<f64, Logical>,
    /// Clock and config captured at restore start, needed to convert the
    /// restore back into a regular level command when user input takes over
    /// the camera mid-flight.
    clock: Clock,
    config: niri_config::Animation,
}

/// Payload of an unlocked direct-manipulation gesture.
///
/// The gesture owns the level directly: gesture updates set the displayed
/// level, with no clock involvement.
#[derive(Debug, Clone)]
pub struct GesturePayload {
    /// The displayed level when the gesture began.
    start_level: f64,
    /// The level set by the latest gesture update.
    current_level: f64,
    /// How the focal point behaves while the level changes.
    focal: GestureFocal,
}

/// Payload of a locked direct-manipulation gesture.
#[derive(Debug, Clone)]
pub struct LockedGesturePayload {
    /// The displayed level when the gesture began.
    start_level: f64,
    /// The level set by the latest gesture update.
    current_level: f64,
    /// How the focal point behaves while the level changes.
    focal: LockedGestureFocal,
}

/// Focal point behavior during an unlocked gesture.
#[derive(Debug, Clone, Copy)]
pub enum GestureFocal {
    /// Keeps the `content` point at the `display` position while the level
    /// changes, subject to the output bounds clamp.
    Anchored(FlexibleAnchor),
    /// The focal point stays fixed while the level changes.
    ///
    /// Reached by locking mid-gesture: the camera freezes at its current
    /// displayed position for the rest of the gesture.
    Fixed { focal: Point<f64, Logical> },
}

/// Focal point behavior during a locked gesture.
#[derive(Debug, Clone, Copy)]
pub enum LockedGestureFocal {
    /// Keeps the content point displayed at the viewport center centered
    /// while the level changes.
    Anchored(LockedAnchor),
    /// The focal point stays fixed while the level changes.
    ///
    /// Reached by locking mid-gesture: the camera freezes at its current
    /// displayed position for the rest of the gesture.
    Fixed { focal: Point<f64, Logical> },
}

/// A resting deadzone follow.
///
/// The level is already at its target; only the focal point moves, driven by
/// the shared distance-driven follow physics towards `to_focal`. A follow is
/// mutually exclusive with the other transitions: it starts only once the
/// level animation, restore or gesture that owned the viewport has finished.
#[derive(Debug, Clone, Copy)]
pub struct FollowState {
    /// The clamped focal point the follow converges to.
    to_focal: Point<f64, Logical>,
    /// Timestamp of the last follow step.
    ///
    /// Owned by the follow itself: it is created with the follow and dies
    /// with it, so a follow can never consume time that elapsed before it
    /// started — while tracking was suspended, the compositor was idle, or a
    /// different transition owned the viewport.
    last_step: Duration,
}

/// A deadzone drift of a command anchor's displayed position.
///
/// While the displayed cursor is outside the deadzone during a level
/// command, the anchor's display position moves towards the deadzone
/// boundary with the shared distance-driven follow physics, so the camera
/// starts following immediately instead of waiting for the level animation
/// to finish. The payload stores enough live geometry — the last step
/// timestamp and the drift's displayed destination — to become a resting
/// [`FollowState`] when the command completes first, without consuming stale
/// time.
#[derive(Debug, Clone, Copy)]
pub struct CombinedFollow {
    /// Timestamp of the last drift step.
    last_step: Duration,
    /// The displayed destination of the drift: the nearest point inside/on
    /// the deadzone at the last evaluation.
    target_display: Point<f64, Logical>,
}

/// Zoom-out results within this distance of 1 are snapped to exactly 1 so
/// that repeated zoom-in/out cycles and pinch gestures do not leave a
/// floating-point residue that keeps the zoom nominally active.
pub(crate) const ZOOM_SNAP_TO_ONE_EPSILON: f64 = 1e-9;

/// The smallest `log2` level delta that carries velocity into a restore's
/// progress animation.
///
/// Below this the level is visually stationary, so the restore starts from
/// rest instead of mapping a huge progress velocity out of a tiny delta.
const RESTORE_VELOCITY_MIN_DELTA: f64 = 1e-3;

/// Which operation created a zoom command.
///
/// The direction is resolved from the user intent by the caller and recorded
/// in the command variant; it does not change the physics.
#[derive(Debug, Clone, Copy)]
enum CommandDirection {
    In,
    Out,
    Value,
}

/// The distance-driven deadzone follow input for one frame.
///
/// Computed from the current displayed geometry only: no momentum or
/// carried velocity is stored between frames.
#[derive(Debug, Clone, Copy)]
struct DeadzoneFollowInput {
    /// Unit vector of the displayed overshoot, pointing away from the
    /// deadzone. The camera moves the displayed point along `-direction`.
    direction: Point<f64, Logical>,
    /// The follow speed in displayed logical pixels per second, derived
    /// from the relative overshoot depth through the smoothstep easing.
    speed: f64,
    /// The nearest point inside/on the deadzone: the displayed destination.
    target: Point<f64, Logical>,
    /// The Euclidean length of the displayed overshoot.
    overshoot_length: f64,
}

impl ZoomCommandState {
    /// The level this command animates towards.
    fn target(&self) -> f64 {
        match self {
            Self::In { target, .. } | Self::Out { target, .. } | Self::Value { target, .. } => {
                *target
            }
            Self::ToIdentity { .. } => 1.,
        }
    }

    fn animation(&self) -> &Animation {
        match self {
            Self::In { payload, .. } | Self::Out { payload, .. } | Self::Value { payload, .. } => {
                &payload.animation
            }
            Self::ToIdentity { payload } => &payload.animation,
        }
    }

    /// The command's anchor, if it has one.
    ///
    /// `ToIdentity` has no anchor: its focal point is fixed.
    fn anchor(&self) -> Option<&FlexibleAnchor> {
        match self {
            Self::In { payload, .. } | Self::Out { payload, .. } | Self::Value { payload, .. } => {
                Some(&payload.anchor)
            }
            Self::ToIdentity { .. } => None,
        }
    }

    fn anchor_mut(&mut self) -> Option<&mut FlexibleAnchor> {
        match self {
            Self::In { payload, .. } | Self::Out { payload, .. } | Self::Value { payload, .. } => {
                Some(&mut payload.anchor)
            }
            Self::ToIdentity { .. } => None,
        }
    }
}

impl ZoomingState {
    /// The mutable anchor of a command, if this is an anchored command.
    fn anchor_mut(&mut self) -> Option<&mut FlexibleAnchor> {
        match self {
            Self::Command(c) => c.anchor_mut(),
            Self::Restore(_) => None,
        }
    }
}

impl LockedCommandState {
    /// The level this command animates towards.
    fn target(&self) -> f64 {
        match self {
            Self::In { target, .. } | Self::Out { target, .. } | Self::Value { target, .. } => {
                *target
            }
            Self::ToIdentity { .. } => 1.,
        }
    }

    fn animation(&self) -> &Animation {
        match self {
            Self::In { payload, .. } | Self::Out { payload, .. } | Self::Value { payload, .. } => {
                &payload.animation
            }
            Self::ToIdentity { payload } => &payload.animation,
        }
    }
}

impl LockedZoomingState {
    /// The level this transition animates towards.
    fn target(&self) -> f64 {
        match self {
            Self::Command(c) => c.target(),
        }
    }

    fn animation(&self) -> &Animation {
        match self {
            Self::Command(c) => c.animation(),
        }
    }
}

impl OutputZoomState {
    /// Creates a zoom state for an output of the given size.
    ///
    /// Starts at 1x zoom, unlocked, with the focal point at the output center.
    ///
    /// # Panics
    ///
    /// Panics if `view_size` is not finite or has a negative component.
    pub fn new(view_size: Size<f64, Logical>) -> Self {
        assert!(view_size.w.is_finite() && view_size.h.is_finite());
        assert!(view_size.w >= 0. && view_size.h >= 0.);

        Self::Idle(ZoomView {
            level: 1.,
            focal: view_size.to_point().downscale(2.),
            view_size,
        })
    }

    /// The view shared by every state.
    fn view(&self) -> &ZoomView {
        match self {
            Self::Idle(view)
            | Self::Follow { view, .. }
            | Self::Zooming { view, .. }
            | Self::ZoomingFollow { view, .. }
            | Self::Gesture { view, .. }
            | Self::Locked(view)
            | Self::LockedZooming { view, .. }
            | Self::LockedGesture { view, .. } => view,
        }
    }

    fn view_mut(&mut self) -> &mut ZoomView {
        match self {
            Self::Idle(view)
            | Self::Follow { view, .. }
            | Self::Zooming { view, .. }
            | Self::ZoomingFollow { view, .. }
            | Self::Gesture { view, .. }
            | Self::Locked(view)
            | Self::LockedZooming { view, .. }
            | Self::LockedGesture { view, .. } => view,
        }
    }

    /// Moves the current state out, leaving a resting `Idle` behind.
    ///
    /// The placeholder's view is a copy of the current one; callers that
    /// need the live camera position must sample `focal()`/`level()` before
    /// taking, since the placeholder only carries the committed fields.
    /// `ZoomView` is `Copy`, so the placeholder is cheap and the real
    /// payload is moved, never cloned.
    fn take(&mut self) -> Self {
        mem::replace(self, Self::Idle(*self.view()))
    }

    /// Builds a command variant classified by the direction of the level
    /// delta: `In` when the target is above the displayed level, `Out` when
    /// below, `Value` when equal. Used for commands synthesized internally
    /// (restore takeovers), where there is no originating operation kind.
    fn command_by_delta(target: f64, level: f64, payload: ZoomCommand) -> ZoomCommandState {
        if target > level {
            ZoomCommandState::In { target, payload }
        } else if target < level {
            ZoomCommandState::Out { target, payload }
        } else {
            ZoomCommandState::Value { target, payload }
        }
    }

    /// The locked counterpart of [`command_by_delta`](Self::command_by_delta).
    fn locked_command_by_delta(
        target: f64,
        level: f64,
        payload: LockedZoomCommand,
    ) -> LockedCommandState {
        if target > level {
            LockedCommandState::In { target, payload }
        } else if target < level {
            LockedCommandState::Out { target, payload }
        } else {
            LockedCommandState::Value { target, payload }
        }
    }

    /// The currently displayed zoom level.
    ///
    /// While a transition is in progress this samples the animation; once the
    /// animation first reaches its target this returns the target exactly.
    pub fn level(&self) -> f64 {
        match self {
            Self::Idle(view) | Self::Follow { view, .. } | Self::Locked(view) => view.level,
            Self::Zooming { zooming, .. } => match zooming {
                ZoomingState::Command(c) => Self::command_level(c),
                ZoomingState::Restore(r) => Self::restore_level(r),
            },
            Self::ZoomingFollow { zooming, .. } => Self::command_level(zooming),
            Self::Gesture { gesture, .. } => gesture.current_level,
            Self::LockedZooming { zooming, .. } => match zooming {
                LockedZoomingState::Command(c) => Self::locked_command_level(c),
            },
            Self::LockedGesture { gesture, .. } => gesture.current_level,
        }
    }

    /// The displayed level of an unlocked command.
    ///
    /// Once the animation first reaches its target this returns the target
    /// exactly rather than `exp2(log2(target))`, so that level == 1.0 stays
    /// exact for the identity fast path. In flight the value is clamped to
    /// the animation endpoints so that an underdamped spring can never
    /// display a level past the target or below the start.
    fn command_level(c: &ZoomCommandState) -> f64 {
        let animation = c.animation();
        if animation.is_clamped_done() {
            return c.target();
        }
        let z = animation.value().clamp(
            animation.from().min(animation.to()),
            animation.from().max(animation.to()),
        );
        z.exp2()
    }

    fn locked_command_level(c: &LockedCommandState) -> f64 {
        let animation = c.animation();
        if animation.is_clamped_done() {
            return c.target();
        }
        let z = animation.value().clamp(
            animation.from().min(animation.to()),
            animation.from().max(animation.to()),
        );
        z.exp2()
    }

    /// The displayed level of a restore.
    ///
    /// The progress animation is clamped to `0..=1`, so the level stays
    /// between the endpoints even for an underdamped spring.
    fn restore_level(r: &RestorePayload) -> f64 {
        if r.animation.is_clamped_done() {
            return r.to_level;
        }
        let p = r.animation.clamped_value().clamp(0., 1.);
        let z = r.from_level.log2() + (r.to_level.log2() - r.from_level.log2()) * p;
        z.exp2()
    }

    /// The user-requested zoom level.
    ///
    /// May differ from [`level()`](Self::level) while a continuous action or
    /// animation is in progress. There is no authoritative target outside an
    /// in-progress operation: at rest this is the committed level.
    pub fn target_level(&self) -> f64 {
        match self {
            Self::Idle(view) | Self::Follow { view, .. } | Self::Locked(view) => view.level,
            Self::Zooming { zooming, .. } => match zooming {
                ZoomingState::Command(c) => c.target(),
                ZoomingState::Restore(r) => r.to_level,
            },
            Self::ZoomingFollow { zooming, .. } => zooming.target(),
            Self::Gesture { gesture, .. } => gesture.current_level,
            Self::LockedGesture { gesture, .. } => gesture.current_level,
            Self::LockedZooming { zooming, .. } => zooming.target(),
        }
    }

    /// The level that incremental zoom actions operate on.
    ///
    /// This is the current user intent: the level `zoom-in`/`zoom-out` and
    /// `zoom` base their next target on. It coincides with
    /// [`target_level()`](Self::target_level): during a gesture it is the
    /// level set by the latest update, during a restore the saved target.
    pub fn intent_level(&self) -> f64 {
        self.target_level()
    }

    /// The fixed point of the current viewport transform, in output-local
    /// logical coordinates.
    ///
    /// While a transition is in progress this is derived from the transition
    /// state at the current animated level.
    pub fn focal(&self) -> Point<f64, Logical> {
        match self {
            Self::Idle(view) | Self::Locked(view) => view.focal,
            Self::Follow { view, .. } => {
                // The distance-driven follow mutates the committed focal
                // point in place every frame.
                Self::clamp_focal(view.focal, view.view_size)
            }
            Self::Zooming { view, zooming } => match zooming {
                ZoomingState::Command(c) => Self::command_focal(view, c),
                ZoomingState::Restore(r) => Self::restore_focal(view, r),
            },
            Self::ZoomingFollow { view, zooming, .. } => Self::command_focal(view, zooming),
            Self::Gesture { view, gesture } => Self::gesture_focal(view, gesture),
            Self::LockedZooming { view, zooming } => match zooming {
                LockedZoomingState::Command(c) => Self::locked_command_focal(view, c),
            },
            Self::LockedGesture { view, gesture } => Self::locked_gesture_focal(view, gesture),
        }
    }

    /// The focal point of an unlocked command at the current level.
    ///
    /// The anchor's display position already carries any deadzone drift: it
    /// is mutated in place by the distance-driven step.
    fn command_focal(view: &ZoomView, c: &ZoomCommandState) -> Point<f64, Logical> {
        match c.anchor() {
            Some(anchor) => {
                Self::anchored_focal(view, anchor.content, anchor.display, Self::command_level(c))
            }
            None => match c {
                ZoomCommandState::ToIdentity { payload } => {
                    Self::clamp_focal(payload.focal, view.view_size)
                }
                _ => unreachable!(),
            },
        }
    }

    fn locked_command_focal(view: &ZoomView, c: &LockedCommandState) -> Point<f64, Logical> {
        match c {
            LockedCommandState::In { payload, .. }
            | LockedCommandState::Out { payload, .. }
            | LockedCommandState::Value { payload, .. } => Self::anchored_focal(
                view,
                payload.anchor.content,
                payload.anchor.display,
                Self::locked_command_level(c),
            ),
            LockedCommandState::ToIdentity { payload } => {
                Self::clamp_focal(payload.focal, view.view_size)
            }
        }
    }

    /// The focal point of a restore at the current progress.
    fn restore_focal(view: &ZoomView, r: &RestorePayload) -> Point<f64, Logical> {
        // Restoring to 1x: the destination focal point is visually
        // degenerate (the transform is the identity), so the focal point
        // stays at `from_focal` until the transition commits. Interpolating
        // it early would pan the viewport towards a point that only matters
        // once the level reaches 1.
        if r.to_level == 1. && !r.animation.is_clamped_done() {
            return Self::clamp_focal(r.from_focal, view.view_size);
        }

        // The focal point interpolates with the clamped progress so that it
        // never overshoots the restore destination.
        let p = r.animation.clamped_value().clamp(0., 1.);
        let to_focal = Self::clamp_focal(r.to_focal, view.view_size);
        let focal = Point::from((
            r.from_focal.x + (to_focal.x - r.from_focal.x) * p,
            r.from_focal.y + (to_focal.y - r.from_focal.y) * p,
        ));
        Self::clamp_focal(focal, view.view_size)
    }

    fn gesture_focal(view: &ZoomView, g: &GesturePayload) -> Point<f64, Logical> {
        match &g.focal {
            GestureFocal::Anchored(anchor) => {
                Self::anchored_focal(view, anchor.content, anchor.display, g.current_level)
            }
            GestureFocal::Fixed { focal } => Self::clamp_focal(*focal, view.view_size),
        }
    }

    fn locked_gesture_focal(view: &ZoomView, g: &LockedGesturePayload) -> Point<f64, Logical> {
        match &g.focal {
            LockedGestureFocal::Anchored(anchor) => {
                Self::anchored_focal(view, anchor.content, anchor.display, g.current_level)
            }
            LockedGestureFocal::Fixed { focal } => Self::clamp_focal(*focal, view.view_size),
        }
    }

    /// Whether focal tracking is locked.
    pub fn is_locked(&self) -> bool {
        matches!(
            self,
            Self::Locked(_) | Self::LockedZooming { .. } | Self::LockedGesture { .. }
        )
    }

    /// Whether a zoom level transition is in progress.
    ///
    /// A gesture is not an animation: gesture events drive the displayed
    /// level directly and queue their own redraws.
    pub fn is_animating(&self) -> bool {
        match self {
            Self::Zooming { zooming, .. } => match zooming {
                ZoomingState::Command(c) => !c.animation().is_clamped_done(),
                ZoomingState::Restore(r) => !r.animation.is_clamped_done(),
            },
            // A combined drift is always active while the state exists, so
            // the state animates until the command completes and the drift
            // is handed off to a resting follow.
            Self::ZoomingFollow { .. } => true,
            Self::LockedZooming { zooming, .. } => !zooming.animation().is_clamped_done(),
            // A distance-driven follow animates until it reaches its target.
            Self::Follow { follow, .. } => self.focal() != follow.to_focal,
            Self::Idle(_) | Self::Gesture { .. } | Self::Locked(_) | Self::LockedGesture { .. } => {
                false
            }
        }
    }

    /// Whether an autonomous level transition is in progress.
    ///
    /// Autonomous means clock-driven: commands, restores and their combined
    /// drift, locked or not. Direct-manipulation gestures and resting
    /// follows are not zooming.
    pub fn is_zooming(&self) -> bool {
        matches!(
            self,
            Self::Zooming { .. } | Self::ZoomingFollow { .. } | Self::LockedZooming { .. }
        )
    }

    /// Whether a deadzone follow currently owns the focal point.
    ///
    /// Covers both the resting follow and the combined drift inside a level
    /// command.
    pub fn is_following(&self) -> bool {
        matches!(self, Self::Follow { .. } | Self::ZoomingFollow { .. })
    }

    /// Whether a direct-manipulation gesture currently owns the zoom level.
    ///
    /// While gesturing, [`level()`](Self::level) and
    /// [`target_level()`](Self::target_level) both report the level set by
    /// the latest gesture update.
    pub fn is_gesturing(&self) -> bool {
        matches!(self, Self::Gesture { .. } | Self::LockedGesture { .. })
    }

    /// The animation driving the current autonomous transition, if any.
    ///
    /// Used by production code that samples animation state and by tests
    /// that verify velocity carry-over.
    fn animation(&self) -> Option<&Animation> {
        match self {
            Self::Zooming { zooming, .. } => match zooming {
                ZoomingState::Command(c) => Some(c.animation()),
                ZoomingState::Restore(r) => Some(&r.animation),
            },
            Self::ZoomingFollow { zooming, .. } => Some(zooming.animation()),
            Self::LockedZooming { zooming, .. } => Some(zooming.animation()),
            Self::Idle(_)
            | Self::Follow { .. }
            | Self::Gesture { .. }
            | Self::Locked(_)
            | Self::LockedGesture { .. } => None,
        }
    }

    /// Sets whether focal tracking is locked.
    ///
    /// `pointer` is the canonical pointer position in output-local content
    /// coordinates; callers pass the output center when the pointer is
    /// elsewhere.
    ///
    /// Locking materializes the current displayed camera position first (a
    /// running follow commits its focal), drops any deadzone drift, and
    /// re-anchors an in-flight command or restore on the viewport center, so
    /// the viewed content stays centered. A gesture freezes the focal point
    /// at its current displayed value instead: the level keeps following the
    /// fingers while the camera stops tracking the anchor.
    ///
    /// Unlocking does not move the viewport: an in-flight command re-anchors
    /// on the canonical pointer at its current displayed position, a
    /// `ToIdentity` command keeps its fixed focal point, and a gesture keeps
    /// its current focal mode. Regular deadzone tracking can move the camera
    /// again on the next pointer event.
    pub fn set_locked(&mut self, locked: bool, pointer: Point<f64, Logical>) {
        if locked {
            self.lock();
        } else {
            self.unlock(pointer);
        }
    }

    /// Toggles whether focal tracking is locked.
    ///
    /// `pointer` is the canonical pointer position used to re-anchor an
    /// in-flight command on unlock.
    pub fn toggle_locked(&mut self, pointer: Point<f64, Logical>) {
        self.set_locked(!self.is_locked(), pointer);
    }

    /// Transitions the state machine into its locked counterpart.
    fn lock(&mut self) {
        let center = self.view().view_size.to_point().downscale(2.);
        let center_content = self.viewport_transform().apply_inverse(center);
        self.lock_at(center_content, center);
    }

    /// Locks with a fixed content/display anchor instead of enabling follow.
    fn lock_at(
        &mut self,
        anchor_content: Point<f64, Logical>,
        anchor_display: Point<f64, Logical>,
    ) {
        let current_focal = self.focal();
        let displayed_level = self.level();

        *self = match self.take() {
            Self::Idle(view) | Self::Follow { view, .. } | Self::Locked(view) => Self::Locked(view),
            Self::Zooming { view, zooming } => {
                let command = match zooming {
                    ZoomingState::Command(c) => {
                        Self::lock_command(c, anchor_content, anchor_display)
                    }
                    ZoomingState::Restore(r) => Self::restore_to_locked_command(
                        r,
                        displayed_level,
                        current_focal,
                        anchor_content,
                        anchor_display,
                    ),
                };
                Self::LockedZooming {
                    view,
                    zooming: LockedZoomingState::Command(command),
                }
            }
            Self::ZoomingFollow { view, zooming, .. } => Self::LockedZooming {
                view,
                zooming: LockedZoomingState::Command(Self::lock_command(
                    zooming,
                    anchor_content,
                    anchor_display,
                )),
            },
            Self::Gesture { view, gesture } => Self::LockedGesture {
                view,
                gesture: LockedGesturePayload {
                    start_level: gesture.start_level,
                    current_level: gesture.current_level,
                    // Locking mid-gesture freezes the focal point at its
                    // current displayed value for the rest of the gesture.
                    focal: LockedGestureFocal::Fixed {
                        focal: current_focal,
                    },
                },
            },
            Self::LockedZooming { view, zooming } => {
                // Already locked: replace the fixed anchor without restarting.
                let zooming = match zooming {
                    LockedZoomingState::Command(c) => {
                        LockedZoomingState::Command(Self::reanchor_locked_command(
                            c,
                            current_focal,
                            anchor_content,
                            anchor_display,
                        ))
                    }
                };
                Self::LockedZooming { view, zooming }
            }
            Self::LockedGesture { view, gesture } => Self::LockedGesture {
                view,
                gesture: LockedGesturePayload {
                    focal: LockedGestureFocal::Fixed {
                        focal: current_focal,
                    },
                    ..gesture
                },
            },
        };
    }

    /// Converts an unlocked command into its locked counterpart.
    ///
    /// The animation is moved over unchanged; only the fixed anchor is replaced.
    fn lock_command(
        c: ZoomCommandState,
        center_content: Point<f64, Logical>,
        center: Point<f64, Logical>,
    ) -> LockedCommandState {
        let anchor = LockedAnchor {
            content: center_content,
            display: center,
        };
        match c {
            ZoomCommandState::In { target, payload } => LockedCommandState::In {
                target,
                payload: LockedZoomCommand {
                    animation: payload.animation,
                    anchor,
                },
            },
            ZoomCommandState::Out { target, payload } => LockedCommandState::Out {
                target,
                payload: LockedZoomCommand {
                    animation: payload.animation,
                    anchor,
                },
            },
            ZoomCommandState::Value { target, payload } => LockedCommandState::Value {
                target,
                payload: LockedZoomCommand {
                    animation: payload.animation,
                    anchor,
                },
            },
            ZoomCommandState::ToIdentity { payload } => LockedCommandState::ToIdentity {
                payload: LockedIdentityCommand {
                    animation: payload.animation,
                    focal: payload.focal,
                },
            },
        }
    }

    /// Re-anchors a locked command on the viewport center.
    ///
    /// Used when locking an already-locked command: the animation is moved
    /// over unchanged and the anchor is replaced by the current center
    /// content. `ToIdentity` keeps a fixed focal point instead.
    fn reanchor_locked_command(
        c: LockedCommandState,
        current_focal: Point<f64, Logical>,
        center_content: Point<f64, Logical>,
        center: Point<f64, Logical>,
    ) -> LockedCommandState {
        let anchor = LockedAnchor {
            content: center_content,
            display: center,
        };
        match c {
            LockedCommandState::In { target, payload } => LockedCommandState::In {
                target,
                payload: LockedZoomCommand {
                    animation: payload.animation,
                    anchor,
                },
            },
            LockedCommandState::Out { target, payload } => LockedCommandState::Out {
                target,
                payload: LockedZoomCommand {
                    animation: payload.animation,
                    anchor,
                },
            },
            LockedCommandState::Value { target, payload } => LockedCommandState::Value {
                target,
                payload: LockedZoomCommand {
                    animation: payload.animation,
                    anchor,
                },
            },
            LockedCommandState::ToIdentity { payload } => LockedCommandState::ToIdentity {
                payload: LockedIdentityCommand {
                    animation: payload.animation,
                    focal: current_focal,
                },
            },
        }
    }

    /// Converts a restore into a locked level command.
    ///
    /// The saved focal destination is abandoned: the level keeps animating
    /// towards the restore target with its current velocity, anchored on the
    /// viewport center.
    fn restore_to_locked_command(
        r: RestorePayload,
        displayed_level: f64,
        current_focal: Point<f64, Logical>,
        center_content: Point<f64, Logical>,
        center: Point<f64, Logical>,
    ) -> LockedCommandState {
        let dz = r.to_level.log2() - r.from_level.log2();
        let velocity = if dz.abs() > RESTORE_VELOCITY_MIN_DELTA {
            r.animation.velocity().unwrap_or(0.) * dz
        } else {
            0.
        };
        let animation = Animation::new(
            r.clock,
            displayed_level.log2(),
            r.to_level.log2(),
            velocity,
            r.config,
        );

        if r.to_level == 1. {
            // Zooming out to the identity transform degenerates the anchored
            // focal solve, so the focal point stays fixed instead.
            return LockedCommandState::ToIdentity {
                payload: LockedIdentityCommand {
                    animation,
                    focal: current_focal,
                },
            };
        }

        let payload = LockedZoomCommand {
            animation,
            anchor: LockedAnchor {
                content: center_content,
                display: center,
            },
        };
        Self::locked_command_by_delta(r.to_level, displayed_level, payload)
    }

    /// Transitions the state machine into its unlocked counterpart.
    ///
    /// `pointer` is the canonical pointer position: an in-flight command
    /// re-anchors on it at its current displayed position, so the camera
    /// does not jump.
    fn unlock(&mut self, pointer: Point<f64, Logical>) {
        // The pointer's displayed position under the current (locked)
        // transform, sampled before the state is taken apart.
        let display = self.viewport_transform().apply(pointer);
        let current_focal = self.focal();

        *self = match self.take() {
            Self::Locked(view) => Self::Idle(view),
            Self::LockedZooming { view, zooming } => {
                let zooming = match zooming {
                    LockedZoomingState::Command(c) => {
                        ZoomingState::Command(Self::unlock_command(c, pointer, display))
                    }
                };
                Self::Zooming { view, zooming }
            }
            Self::LockedGesture { view, gesture } => Self::Gesture {
                view,
                gesture: GesturePayload {
                    start_level: gesture.start_level,
                    current_level: gesture.current_level,
                    // Unlocking mid-gesture keeps the current focal mode: a
                    // frozen focal stays frozen, a center anchor becomes a
                    // regular anchor at the same points.
                    focal: match gesture.focal {
                        LockedGestureFocal::Anchored(anchor) => {
                            GestureFocal::Anchored(FlexibleAnchor {
                                content: anchor.content,
                                display: anchor.display,
                            })
                        }
                        LockedGestureFocal::Fixed { .. } => GestureFocal::Fixed {
                            focal: current_focal,
                        },
                    },
                },
            },
            // A resting follow commits its live focal point, matching the
            // materialize-on-lock-change contract.
            Self::Follow { view, .. } => Self::Idle(view),
            // Unlocking an already-unlocked state is a no-op.
            state => state,
        };
    }

    /// Converts a locked command into its unlocked counterpart.
    ///
    /// The animation is moved over unchanged; the anchor is replaced by the
    /// canonical pointer at its current displayed position. `ToIdentity`
    /// keeps its fixed focal point: the identity solve is degenerate, so
    /// re-anchoring on the pointer would only move the camera.
    fn unlock_command(
        c: LockedCommandState,
        pointer: Point<f64, Logical>,
        display: Point<f64, Logical>,
    ) -> ZoomCommandState {
        let anchor = FlexibleAnchor {
            content: pointer,
            display,
        };
        match c {
            LockedCommandState::In { target, payload } => ZoomCommandState::In {
                target,
                payload: ZoomCommand {
                    animation: payload.animation,
                    anchor,
                },
            },
            LockedCommandState::Out { target, payload } => ZoomCommandState::Out {
                target,
                payload: ZoomCommand {
                    animation: payload.animation,
                    anchor,
                },
            },
            LockedCommandState::Value { target, payload } => ZoomCommandState::Value {
                target,
                payload: ZoomCommand {
                    animation: payload.animation,
                    anchor,
                },
            },
            LockedCommandState::ToIdentity { payload } => ZoomCommandState::ToIdentity {
                payload: IdentityCommand {
                    animation: payload.animation,
                    focal: payload.focal,
                },
            },
        }
    }

    /// Sets the user-requested zoom level and starts a transition towards it
    /// from the currently displayed level.
    ///
    /// `anchor` is a content position in output-local logical coordinates,
    /// typically the cursor. While unlocked, the transition keeps the anchor
    /// at its current displayed position where the output bounds allow it.
    /// While locked, the anchor is ignored and the viewport center stays
    /// fixed instead.
    ///
    /// If a transition is already in progress, the new animation starts from
    /// the currently displayed level and preserves the current velocity when
    /// the animation kind supports it.
    ///
    /// With `config.off` the level is set immediately, like
    /// [`set_level_immediate()`](Self::set_level_immediate).
    ///
    /// # Panics
    ///
    /// Panics if `level` is not finite or less than 1.
    pub fn set_target_level(
        &mut self,
        level: f64,
        anchor: Point<f64, Logical>,
        clock: &Clock,
        config: niri_config::Animation,
    ) {
        assert!(level.is_finite() && level >= 1.);
        self.start_command(level, anchor, clock, config, CommandDirection::Value);
    }

    /// Activates a pointer-anchored preset and locks its camera immediately.
    /// No intermediate unlocked state is presented.
    pub fn set_target_level_and_lock(
        &mut self,
        level: f64,
        anchor: Point<f64, Logical>,
        clock: &Clock,
        config: niri_config::Animation,
    ) {
        let display = self.viewport_transform().apply(anchor);
        self.unlock(anchor);
        self.set_target_level(level, anchor, clock, config);
        self.lock_at(anchor, display);
    }

    /// Starts a `zoom-in` command towards `target`.
    ///
    /// `target` is the level resolved from the current intent by the caller
    /// (typically `intent_level() * factor`, clamped and snapped). A target
    /// of exactly 1 produces a [`ZoomCommandState::ToIdentity`] command like
    /// every other transition to the identity transform.
    ///
    /// # Panics
    ///
    /// Panics if `target` is not finite or less than 1.
    pub fn zoom_in(
        &mut self,
        target: f64,
        anchor: Point<f64, Logical>,
        clock: &Clock,
        config: niri_config::Animation,
    ) {
        assert!(target.is_finite() && target >= 1.);
        debug_assert!(
            target >= self.intent_level(),
            "zoom-in target {target} below intent {}",
            self.intent_level()
        );
        self.start_command(target, anchor, clock, config, CommandDirection::In);
    }

    /// Starts a `zoom-out` command towards `target`.
    ///
    /// `target` is the level resolved from the current intent by the caller.
    /// A target of exactly 1 produces a `ToIdentity` command.
    ///
    /// # Panics
    ///
    /// Panics if `target` is not finite or less than 1.
    pub fn zoom_out(
        &mut self,
        target: f64,
        anchor: Point<f64, Logical>,
        clock: &Clock,
        config: niri_config::Animation,
    ) {
        assert!(target.is_finite() && target >= 1.);
        debug_assert!(
            target <= self.intent_level(),
            "zoom-out target {target} above intent {}",
            self.intent_level()
        );
        self.start_command(target, anchor, clock, config, CommandDirection::Out);
    }

    /// Starts a level command towards `target`.
    ///
    /// Shared implementation of the zoom commands. A resting state already
    /// at `target` is a no-op; otherwise the current log-space velocity
    /// carries over where the animation kind supports it. An active follow
    /// is preserved as the command's combined drift — the camera keeps
    /// following during the level animation — except for `ToIdentity`, which
    /// drops it.
    fn start_command(
        &mut self,
        target: f64,
        anchor: Point<f64, Logical>,
        clock: &Clock,
        config: niri_config::Animation,
        direction: CommandDirection,
    ) {
        // A resting state already at the target has nothing to do: no
        // degenerate animation is allocated and a running follow is not
        // disturbed. In-flight transitions still retarget normally so a
        // repeated action keeps its restart semantics.
        match self {
            Self::Idle(view) | Self::Follow { view, .. } | Self::Locked(view)
                if view.level == target =>
            {
                return;
            }
            _ => (),
        }

        if config.off {
            let pointer_anchored_lock = self.is_locked()
                && matches!(direction, CommandDirection::In | CommandDirection::Out);
            if pointer_anchored_lock {
                self.unlock(anchor);
            }
            self.set_level_immediate(target, anchor);
            if pointer_anchored_lock {
                self.lock();
            }
            return;
        }

        let from_level = self.level();
        let velocity = self.transition_log_velocity();
        let animation = Animation::new(
            clock.clone(),
            from_level.log2(),
            target.log2(),
            velocity,
            config,
        );

        if self.is_locked() {
            // Incremental commands capture the pointer; absolute commands
            // and restores retain their viewport-center policy.
            let (content, display) = match direction {
                CommandDirection::In | CommandDirection::Out => {
                    (anchor, self.viewport_transform().apply(anchor))
                }
                CommandDirection::Value => {
                    let center = self.view().view_size.to_point().downscale(2.);
                    (self.viewport_transform().apply_inverse(center), center)
                }
            };
            let view = *self.view();

            let command = if target == 1. {
                // Zooming out to the identity transform: the anchored focal
                // solve degenerates as the level approaches 1, so the focal
                // point stays fixed and the zoom-out pivots around its
                // displayed position.
                LockedCommandState::ToIdentity {
                    payload: LockedIdentityCommand {
                        animation,
                        focal: self.focal(),
                    },
                }
            } else {
                let payload = LockedZoomCommand {
                    animation,
                    anchor: LockedAnchor { content, display },
                };
                match direction {
                    CommandDirection::In => LockedCommandState::In { target, payload },
                    CommandDirection::Out => LockedCommandState::Out { target, payload },
                    CommandDirection::Value => LockedCommandState::Value { target, payload },
                }
            };
            *self = Self::LockedZooming {
                view,
                zooming: LockedZoomingState::Command(command),
            };
            return;
        }

        // The anchor's displayed position and the fixed focal point are
        // derived from the live state, so sample them before taking it
        // apart.
        let anchor_display = self.viewport_transform().apply(anchor);
        let displayed_focal = self.focal();

        let command = if target == 1. {
            ZoomCommandState::ToIdentity {
                payload: IdentityCommand {
                    animation,
                    focal: displayed_focal,
                },
            }
        } else {
            let payload = ZoomCommand {
                animation,
                anchor: FlexibleAnchor {
                    content: anchor,
                    display: anchor_display,
                },
            };
            match direction {
                CommandDirection::In => ZoomCommandState::In { target, payload },
                CommandDirection::Out => ZoomCommandState::Out { target, payload },
                CommandDirection::Value => ZoomCommandState::Value { target, payload },
            }
        };

        *self = match (self.take(), command) {
            // A resting follow becomes the command's combined drift: the
            // camera keeps following during the level animation instead of
            // committing and restarting. The drift destination is the
            // display position that resolves to the follow's focal target at
            // the current level, and the step timestamp carries over so no
            // stale time is consumed.
            (
                Self::Follow { view, follow },
                command @ (ZoomCommandState::In { .. }
                | ZoomCommandState::Out { .. }
                | ZoomCommandState::Value { .. }),
            ) => Self::ZoomingFollow {
                view,
                zooming: command,
                follow: CombinedFollow {
                    last_step: follow.last_step,
                    target_display: Point::from((
                        view.level * anchor.x - follow.to_focal.x * (view.level - 1.),
                        view.level * anchor.y - follow.to_focal.y * (view.level - 1.),
                    )),
                },
            },
            // A combined drift survives a retarget: the new command keeps
            // the live timing and destination.
            (
                Self::ZoomingFollow { view, follow, .. },
                command @ (ZoomCommandState::In { .. }
                | ZoomCommandState::Out { .. }
                | ZoomCommandState::Value { .. }),
            ) => Self::ZoomingFollow {
                view,
                zooming: command,
                follow,
            },
            // Every other state — including a follow or drift interrupted by
            // a `ToIdentity` command — becomes a plain zooming state.
            (state, command) => Self::Zooming {
                view: *state.view(),
                zooming: ZoomingState::Command(command),
            },
        };
    }

    /// The viewport transform for the currently displayed zoom level.
    pub fn viewport_transform(&self) -> ViewportTransform {
        ViewportTransform::new(self.focal(), self.level())
    }

    /// The logical viewport: the part of the output's content currently
    /// magnified to fill the whole output.
    ///
    /// Its size is `view_size / level`. When `level > 1` the viewport is
    /// always contained within the output rectangle.
    pub fn viewport(&self) -> Rectangle<f64, Logical> {
        self.viewport_transform()
            .apply_inverse_rect(Rectangle::from_size(self.view().view_size))
    }

    /// The deadzone rectangle in displayed coordinates.
    ///
    /// `deadzone_size` is the fraction of the output size along each axis. The
    /// deadzone is always centered on the output: `0` degenerates to the
    /// center point, `1` covers the whole output.
    ///
    /// # Panics
    ///
    /// Panics if `deadzone_size` is not finite or outside `[0, 1]`, or if
    /// `view_size` is not finite.
    pub fn deadzone_rect(
        view_size: Size<f64, Logical>,
        deadzone_size: f64,
    ) -> Rectangle<f64, Logical> {
        assert!(deadzone_size.is_finite() && (0. ..=1.).contains(&deadzone_size));
        assert!(view_size.w.is_finite() && view_size.h.is_finite());

        let size = view_size.upscale(deadzone_size);
        let loc = (view_size.to_point() - size.to_point()).downscale(2.);
        Rectangle::new(loc, size)
    }

    /// The focal point that brings the displayed cursor back to the nearest
    /// deadzone boundary, clamped to the output.
    ///
    /// `cursor` is the cursor position in output-local logical coordinates.
    /// Returns `None` when no camera movement is required: at `level == 1`,
    /// while the displayed cursor is inside the deadzone, or when the clamped
    /// target equals the currently displayed focal point (including the case
    /// where the viewport clamp makes further correction impossible).
    ///
    /// # Panics
    ///
    /// Panics if `deadzone_size` is not finite or outside `[0, 1]`.
    fn focal_target_for_cursor(
        &self,
        cursor: Point<f64, Logical>,
        deadzone_size: f64,
    ) -> Option<Point<f64, Logical>> {
        let level = self.level();
        if level == 1. {
            return None;
        }

        let deadzone = Self::deadzone_rect(self.view().view_size, deadzone_size);
        let display = self.viewport_transform().apply(cursor);
        if deadzone.contains(display) {
            return None;
        }

        let desired = Point::<f64, Logical>::from((
            display
                .x
                .clamp(deadzone.loc.x, deadzone.loc.x + deadzone.size.w),
            display
                .y
                .clamp(deadzone.loc.y, deadzone.loc.y + deadzone.size.h),
        ));

        // The focal point that places the content cursor at the desired
        // displayed position: display = focal + (cursor - focal) * level.
        let focal = Point::from((
            (level * cursor.x - desired.x) / (level - 1.),
            (level * cursor.y - desired.y) / (level - 1.),
        ));
        let focal = Self::clamp_focal(focal, self.view().view_size);

        (focal != self.focal()).then_some(focal)
    }

    /// The distance-driven follow input for a displayed point.
    ///
    /// `display` is a point in displayed output-local logical coordinates,
    /// typically the displayed cursor. Returns `None` when the point is
    /// inside or on the deadzone: there is no overshoot and no follow.
    ///
    /// The intensity is the rectangular `L∞` relative depth of the
    /// overshoot: each axis is normalized by the space available from the
    /// crossed deadzone border to the output edge, and the intensity is the
    /// maximum of the two. The direction is the Euclidean unit vector of the
    /// overshoot. Keeping them separate means a diagonal overshoot at the
    /// same relative depth produces the same speed as a single-axis one —
    /// no `√2` boost.
    fn deadzone_follow_input(
        view_size: Size<f64, Logical>,
        deadzone_size: f64,
        display: Point<f64, Logical>,
        min_speed: f64,
        max_speed: f64,
    ) -> Option<DeadzoneFollowInput> {
        let deadzone = Self::deadzone_rect(view_size, deadzone_size);

        // The nearest point inside/on the deadzone and the overshoot vector.
        let target = Point::from((
            display
                .x
                .clamp(deadzone.loc.x, deadzone.loc.x + deadzone.size.w),
            display
                .y
                .clamp(deadzone.loc.y, deadzone.loc.y + deadzone.size.h),
        ));
        let overshoot = display - target;

        // The space available for the overshoot on each axis, measured from
        // the crossed deadzone border to the output edge in the overshoot
        // direction. An axis without room (e.g. deadzone-size 1) contributes
        // nothing and can never divide by zero.
        let available_x = if overshoot.x > 0. {
            view_size.w - (deadzone.loc.x + deadzone.size.w)
        } else if overshoot.x < 0. {
            deadzone.loc.x
        } else {
            0.
        };
        let available_y = if overshoot.y > 0. {
            view_size.h - (deadzone.loc.y + deadzone.size.h)
        } else if overshoot.y < 0. {
            deadzone.loc.y
        } else {
            0.
        };

        let nx = if available_x > 0. {
            (overshoot.x.abs() / available_x).clamp(0., 1.)
        } else {
            0.
        };
        let ny = if available_y > 0. {
            (overshoot.y.abs() / available_y).clamp(0., 1.)
        } else {
            0.
        };
        let intensity = nx.max(ny);

        let length = (overshoot.x * overshoot.x + overshoot.y * overshoot.y).sqrt();
        if intensity == 0. || length == 0. {
            return None;
        }

        // smoothstep(t) = t²(3 − 2t): gentle near the border, approaching
        // max speed smoothly near the output edge.
        let e = intensity * intensity * (3. - 2. * intensity);
        let speed = min_speed + (max_speed - min_speed) * e;

        Some(DeadzoneFollowInput {
            direction: Point::from((overshoot.x / length, overshoot.y / length)),
            speed,
            target,
            overshoot_length: length,
        })
    }

    /// Evaluates deadzone tracking for the cursor.
    ///
    /// `cursor` is the cursor position in output-local logical coordinates.
    /// While the displayed cursor stays inside the deadzone the focal point
    /// does not move. Once it leaves, the camera smoothly follows the cursor
    /// until the displayed cursor reaches the deadzone edge or the viewport
    /// clamp makes further correction impossible.
    ///
    /// Pointer interaction during a restore takes over the camera: the
    /// restore converts to a regular level command anchored at the cursor's
    /// current displayed position, so the level keeps animating towards the
    /// restore target without a focal jump. During a level command the
    /// anchor owns the camera, but tracking still applies as a drift of the
    /// anchor's display position towards the deadzone edge, so the camera
    /// starts following the cursor during the zoom animation rather than
    /// after it.
    ///
    /// Does nothing while [`is_locked()`](Self::is_locked), at `level == 1`,
    /// or while a gesture owns the viewport.
    ///
    /// Returns `true` if the state changed.
    ///
    /// # Panics
    ///
    /// Panics if `deadzone_size` is not finite or outside `[0, 1]`.
    pub fn update_focal_for_cursor(
        &mut self,
        cursor: Point<f64, Logical>,
        zoom: niri_config::Zoom,
        clock: &Clock,
        config: niri_config::Animation,
    ) -> bool {
        // A gesture owns the viewport: the anchor is fixed at gesture begin
        // and must not be re-pinned by pointer motion.
        if self.is_gesturing() {
            return false;
        }

        if self.is_locked() || self.level() == 1. {
            return false;
        }

        // During a level command the anchor owns the camera: retarget the
        // deadzone drift (or stop it when the cursor re-enters the deadzone)
        // instead of evaluating a resting-level follow.
        if matches!(
            self,
            Self::Zooming {
                zooming: ZoomingState::Command(_),
                ..
            } | Self::ZoomingFollow { .. }
        ) {
            return self.update_follow(cursor, zoom, clock, config);
        }

        let Some(_) = self.focal_target_for_cursor(cursor, zoom.deadzone_size) else {
            // The displayed cursor is inside the deadzone or already at the
            // target: an active follow has nothing left to do.
            return self.commit_follow_focal();
        };

        // During a restore the camera is heading back to a saved focal point.
        // User-driven tracking takes over: the restore becomes a regular level
        // command anchored on the cursor at its current displayed position,
        // so the level keeps animating towards the restore target while the
        // camera stays continuous. The follow evaluation brings the cursor to
        // the deadzone edge once the level animation completes.
        if matches!(
            self,
            Self::Zooming {
                zooming: ZoomingState::Restore(_),
                ..
            }
        ) {
            let display = self.viewport_transform().apply(cursor);
            self.convert_restore_to_command(FlexibleAnchor {
                content: cursor,
                display,
            });
            return true;
        }

        self.update_follow(cursor, zoom, clock, config)
    }

    /// Starts or retargets a deadzone follow for the cursor.
    ///
    /// This is the per-frame evaluation entry point. During a level command
    /// it retargets the anchor's deadzone drift instead of starting a
    /// resting-level follow; a restore or gesture is never disturbed. When
    /// the displayed cursor is inside the deadzone or already at the clamped
    /// target, an active follow commits its current displayed focal point
    /// and stops.
    ///
    /// With `config.off` the focal point is set immediately instead of
    /// starting a follow.
    ///
    /// Returns `true` if the state changed.
    pub fn update_follow(
        &mut self,
        cursor: Point<f64, Logical>,
        zoom: niri_config::Zoom,
        clock: &Clock,
        config: niri_config::Animation,
    ) -> bool {
        let now = clock.now_unadjusted();

        if self.is_locked() {
            // A locked camera does not track the pointer: no deadzone drift
            // during a level transition, no resting-level follow.
            return false;
        }

        match self {
            // A level command owns the camera through its anchor: retarget
            // the deadzone drift instead of starting a resting-level follow.
            Self::Zooming {
                zooming: ZoomingState::Command(_),
                ..
            }
            | Self::ZoomingFollow { .. } => {
                return self.update_display_follow(cursor, zoom, clock, config);
            }
            // A restore or gesture owns the viewport.
            Self::Zooming {
                zooming: ZoomingState::Restore(_),
                ..
            }
            | Self::Gesture { .. }
            | Self::Locked(_)
            | Self::LockedZooming { .. }
            | Self::LockedGesture { .. } => return false,
            Self::Idle(_) | Self::Follow { .. } => (),
        }

        if self.level() == 1. {
            return self.commit_follow_focal();
        }

        let Some(target) = self.focal_target_for_cursor(cursor, zoom.deadzone_size) else {
            // Inside the deadzone or already at the clamped target: commit
            // the current displayed focal point without a jump.
            return self.commit_follow_focal();
        };

        if config.off || clock.should_complete_instantly() {
            return self.set_current_focal(target);
        }

        let display = self.viewport_transform().apply(cursor);
        let input = Self::deadzone_follow_input(
            self.view().view_size,
            zoom.deadzone_size,
            display,
            zoom.follow_min_speed,
            zoom.follow_max_speed,
        );

        // The distance-driven step: speed comes from the current displayed
        // overshoot, direction from the remaining path to the target (which
        // is parallel to the overshoot). The step is capped at the target so
        // the camera can never cross the deadzone border.
        let Some(input) = input else {
            self.view_mut().focal = target;
            self.commit_follow_focal();
            return true;
        };

        // Start a follow if none is active, then apply the distance-driven
        // step for the elapsed time since the previous step. The timestamp
        // is owned by the follow: a new follow starts at `dt = 0` and can
        // never consume time that elapsed before it was created.
        if !matches!(self, Self::Follow { .. }) {
            *self = Self::Follow {
                view: *self.view(),
                follow: FollowState {
                    to_focal: target,
                    last_step: now,
                },
            };
        }
        let Self::Follow { view, follow } = self else {
            unreachable!();
        };

        // The follow target tracks the live cursor.
        follow.to_focal = target;

        let dt = now.saturating_sub(follow.last_step).as_secs_f64();
        follow.last_step = now;
        let distance = input.speed * dt;
        let remaining = target - view.focal;
        let rem_len = (remaining.x * remaining.x + remaining.y * remaining.y).sqrt();

        if distance >= rem_len {
            view.focal = target;
            *self = Self::Idle(*view);
        } else {
            let scale = distance / rem_len;
            view.focal = Point::from((
                view.focal.x + remaining.x * scale,
                view.focal.y + remaining.y * scale,
            ));
        }
        true
    }

    /// Retargets the deadzone drift of an in-progress level command.
    ///
    /// While the displayed cursor is outside the deadzone, the anchor is
    /// re-pinned on the live cursor and its display position steps towards
    /// the deadzone boundary with the shared distance-driven physics, so the
    /// camera follows the cursor during the level animation rather than
    /// after it. When the cursor re-enters the deadzone the drift stops:
    /// the display position is already live, so nothing needs materializing.
    ///
    /// Does nothing unless the current state is an anchored level command.
    /// Returns `true` if the state changed.
    fn update_display_follow(
        &mut self,
        cursor: Point<f64, Logical>,
        zoom: niri_config::Zoom,
        clock: &Clock,
        config: niri_config::Animation,
    ) -> bool {
        let display = self.viewport_transform().apply(cursor);
        let input = Self::deadzone_follow_input(
            self.view().view_size,
            zoom.deadzone_size,
            display,
            zoom.follow_min_speed,
            zoom.follow_max_speed,
        );

        // The drift only applies while the command zooms in: at a target
        // level of 1 the deadzone is meaningless.
        let drift_allowed = self.target_level() > 1. && input.is_some();

        // Whether the drift target is reachable at the current level: when
        // the output clamp already pins the camera, the anchor display
        // position cannot move and no drift is started.
        let reachable = drift_allowed
            && Self::anchored_focal(self.view(), cursor, input.unwrap().target, self.level())
                != self.focal();

        let now = clock.now_unadjusted();

        if !drift_allowed || !reachable {
            // The display position is live: dropping the drift marker stops
            // the camera exactly where it is.
            return self.collapse_drift();
        }
        let input = input.unwrap();

        if config.off || clock.should_complete_instantly() {
            // Instant completion: the drift jumps straight to the deadzone
            // border.
            if let Some(anchor) = self.command_anchor_mut() {
                anchor.content = cursor;
                anchor.display = input.target;
            }
            self.collapse_drift();
            return true;
        }

        match self {
            Self::Zooming { zooming, .. } => {
                // Start the drift: the display position is re-seeded from the
                // live geometry so the first step never consumes time that
                // elapsed before the drift existed.
                let Some(anchor) = zooming.anchor_mut() else {
                    return false;
                };
                anchor.content = cursor;
                anchor.display = display;
                let view = *self.view();
                let Self::Zooming { zooming, .. } = self.take() else {
                    unreachable!();
                };
                let ZoomingState::Command(command) = zooming else {
                    unreachable!();
                };
                *self = Self::ZoomingFollow {
                    view,
                    zooming: command,
                    follow: CombinedFollow {
                        last_step: now,
                        target_display: input.target,
                    },
                };
                true
            }
            Self::ZoomingFollow {
                zooming, follow, ..
            } => {
                // Re-anchor on the live cursor: the camera pans so that the
                // cursor's displayed position drifts towards the deadzone
                // boundary.
                let Some(anchor) = zooming.anchor_mut() else {
                    return false;
                };
                anchor.content = cursor;
                follow.target_display = input.target;

                let dt = now.saturating_sub(follow.last_step).as_secs_f64();
                follow.last_step = now;

                let distance = input.speed * dt;
                if distance >= input.overshoot_length {
                    anchor.display = input.target;
                    // The drift landed: the state collapses back to a plain
                    // command.
                    self.collapse_drift();
                } else {
                    anchor.display = Point::from((
                        display.x - input.direction.x * distance,
                        display.y - input.direction.y * distance,
                    ));
                }
                true
            }
            _ => false,
        }
    }

    /// The mutable anchor of the in-progress unlocked command, if any.
    fn command_anchor_mut(&mut self) -> Option<&mut FlexibleAnchor> {
        match self {
            Self::Zooming {
                zooming: ZoomingState::Command(c),
                ..
            }
            | Self::ZoomingFollow { zooming: c, .. } => c.anchor_mut(),
            _ => None,
        }
    }

    /// Drops the combined drift marker, collapsing `ZoomingFollow` back to a
    /// plain `Zooming` command.
    ///
    /// The anchor's display position is live, so dropping the marker stops
    /// the camera exactly where it is. Returns `true` if a drift was active.
    fn collapse_drift(&mut self) -> bool {
        if !matches!(self, Self::ZoomingFollow { .. }) {
            return false;
        }
        let Self::ZoomingFollow { view, zooming, .. } = self.take() else {
            unreachable!();
        };
        *self = Self::Zooming {
            view,
            zooming: ZoomingState::Command(zooming),
        };
        true
    }

    /// Commits an active follow's current displayed focal point.
    ///
    /// The distance-driven follow mutates the committed focal point in
    /// place every frame, so committing only drops the follow state: the
    /// camera stays exactly where it is. Does nothing unless a resting
    /// follow is active.
    ///
    /// Returns `true` if a follow was committed.
    pub fn commit_follow_focal(&mut self) -> bool {
        if !matches!(self, Self::Follow { .. }) {
            return false;
        }

        // `take` leaves `Idle(view)` behind: the follow's live focal point
        // is already the committed one.
        self.take();
        true
    }

    /// Suspends deadzone follow activity without moving the camera.
    ///
    /// Called by the per-frame driver for outputs where tracking is
    /// suspended or the pointer is elsewhere: commits an active resting
    /// follow at its current displayed focal point and retires an
    /// in-transition deadzone drift. The drift's display position is live,
    /// so dropping it freezes the camera exactly where it is; on resume the
    /// next evaluation re-seeds the drift from the current geometry instead
    /// of consuming the suspended time as one step.
    ///
    /// Returns `true` if the state changed.
    pub fn suspend_follow(&mut self) -> bool {
        let drift = self.collapse_drift();
        self.commit_follow_focal() || drift
    }

    /// Sets the focal point that the current state should display.
    ///
    /// Without a transition this writes the committed focal point. During an
    /// anchored transition it re-pins the anchor's display position so that
    /// the anchored focal resolves to `focal` right now.
    fn set_current_focal(&mut self, focal: Point<f64, Logical>) -> bool {
        let view_size = self.view().view_size;
        let focal = Self::clamp_focal(focal, view_size);
        if self.focal() == focal {
            return false;
        }

        // A follow has no anchor to re-pin: materialize its current
        // displayed focal point first so the write lands on the committed
        // focal point.
        self.commit_follow_focal();

        // A restore has no anchor to re-pin; it converts to a regular level
        // command first so that the focal point can be set directly. The
        // synthesized anchor pins the current focal point at the displayed
        // position that resolves to the requested focal.
        if matches!(
            self,
            Self::Zooming {
                zooming: ZoomingState::Restore(_),
                ..
            }
        ) {
            let level = self.level();
            let current = self.focal();
            self.convert_restore_to_command(FlexibleAnchor {
                content: current,
                display: Point::from((
                    level * current.x - focal.x * (level - 1.),
                    level * current.y - focal.y * (level - 1.),
                )),
            });
        }

        let level = self.level();
        match self {
            Self::Zooming {
                zooming: ZoomingState::Command(c),
                ..
            }
            | Self::ZoomingFollow { zooming: c, .. } => {
                match c {
                    ZoomCommandState::In { payload, .. }
                    | ZoomCommandState::Out { payload, .. }
                    | ZoomCommandState::Value { payload, .. } => {
                        // Solve display = level * content - focal * (level - 1)
                        // so that the anchored focal resolves to `focal`
                        // right now. A deadzone drift is dropped: the
                        // explicit focal wins.
                        payload.anchor.display = Point::from((
                            level * payload.anchor.content.x - focal.x * (level - 1.),
                            level * payload.anchor.content.y - focal.y * (level - 1.),
                        ));
                    }
                    ZoomCommandState::ToIdentity { payload } => payload.focal = focal,
                }
                self.collapse_drift();
            }
            Self::Gesture { gesture, .. } => match &mut gesture.focal {
                GestureFocal::Anchored(anchor) => {
                    anchor.display = Point::from((
                        level * anchor.content.x - focal.x * (level - 1.),
                        level * anchor.content.y - focal.y * (level - 1.),
                    ));
                }
                GestureFocal::Fixed { focal: fixed } => *fixed = focal,
            },
            Self::LockedGesture { gesture, .. } => match &mut gesture.focal {
                LockedGestureFocal::Anchored(anchor) => {
                    anchor.display = Point::from((
                        level * anchor.content.x - focal.x * (level - 1.),
                        level * anchor.content.y - focal.y * (level - 1.),
                    ));
                }
                LockedGestureFocal::Fixed { focal: fixed } => *fixed = focal,
            },
            Self::LockedZooming { zooming, .. } => match zooming {
                LockedZoomingState::Command(c) => match c {
                    LockedCommandState::In { payload, .. }
                    | LockedCommandState::Out { payload, .. }
                    | LockedCommandState::Value { payload, .. } => {
                        payload.anchor.display = Point::from((
                            level * payload.anchor.content.x - focal.x * (level - 1.),
                            level * payload.anchor.content.y - focal.y * (level - 1.),
                        ));
                    }
                    LockedCommandState::ToIdentity { payload } => payload.focal = focal,
                },
            },
            Self::Zooming {
                zooming: ZoomingState::Restore(_),
                ..
            } => unreachable!(),
            Self::Idle(view) | Self::Follow { view, .. } | Self::Locked(view) => {
                view.focal = focal;
            }
        }

        true
    }

    /// Immediately sets the zoom level, keeping `anchor` at its current
    /// displayed position where the output bounds allow it.
    ///
    /// `anchor` is a content position in output-local logical coordinates,
    /// typically the cursor; while locked it is ignored and the viewport
    /// center is kept fixed instead. Also sets the intent level to `level`
    /// and cancels any transition in progress.
    ///
    /// At `level == 1` the transform is the identity, so the displayed
    /// position of `anchor` cannot be preserved; the focal point is still kept
    /// finite and within the output.
    ///
    /// # Panics
    ///
    /// Panics if `level` is not finite or less than 1.
    pub fn set_level_immediate(&mut self, level: f64, anchor: Point<f64, Logical>) {
        assert!(level.is_finite() && level >= 1.);

        // A follow owns the focal point: materialize its current displayed
        // value so the camera position is not lost with the transition.
        self.commit_follow_focal();

        // A locked camera does not track the pointer: zoom around the
        // viewport center so the viewed content stays centered.
        let anchor = if self.is_locked() {
            let center = self.view().view_size.to_point().downscale(2.);
            self.viewport_transform().apply_inverse(center)
        } else {
            anchor
        };

        let screen_anchor = self.viewport_transform().apply(anchor);
        let view_size = self.view().view_size;

        let mut view = *self.view();
        view.level = level;

        if level == 1. {
            view.focal = Self::clamp_focal(view.focal, view_size);
        } else {
            // The focal point that keeps the content anchor at its previous
            // displayed position:
            // screen_anchor = focal + (anchor - focal) * level.
            let focal = Point::from((
                (level * anchor.x - screen_anchor.x) / (level - 1.),
                (level * anchor.y - screen_anchor.y) / (level - 1.),
            ));
            view.focal = Self::clamp_focal(focal, view_size);
        }

        *self = if self.is_locked() {
            Self::Locked(view)
        } else {
            Self::Idle(view)
        };
    }

    /// Ends the zoom session entirely: the state becomes the resting 1x
    /// identity with no transition and no lock.
    ///
    /// This is a lifecycle boundary, not a level change: any in-progress
    /// level animation, restore, gesture or follow is dropped rather than
    /// completed, and the lock is cleared. The focal point keeps its current
    /// displayed value clamped to the output; at the identity transform it
    /// is presentation-degenerate.
    ///
    /// Used when a compositor mode that replaces the desktop presentation
    /// (the Overview) takes over: the zoom session does not survive it and
    /// is not restored afterwards.
    pub fn end_session(&mut self) {
        let view = ZoomView {
            level: 1.,
            focal: Self::clamp_focal(self.focal(), self.view().view_size),
            view_size: self.view().view_size,
        };
        *self = Self::Idle(view);
    }

    /// Begins a direct-manipulation gesture on the zoom level.
    ///
    /// `anchor` is a content position in output-local logical coordinates,
    /// typically the cursor. The currently displayed level becomes the
    /// gesture base: a transition in progress is abandoned at its current
    /// displayed state, including a restore's saved destination. While
    /// unlocked, the anchor is kept at its current displayed position like a
    /// regular zoom transition; while locked, the viewport center stays fixed.
    ///
    /// The intent level is set to the displayed level: during a gesture the
    /// displayed state is the current user intent.
    pub fn begin_gesture(&mut self, anchor: Point<f64, Logical>) {
        // A follow owns the focal point: materialize its current displayed
        // value so the gesture takes over the real camera position.
        self.commit_follow_focal();

        let level = self.level();
        let view = *self.view();

        if self.is_locked() {
            // A locked camera does not track the pointer: zoom around the
            // viewport center so the viewed content stays centered.
            let center = view.view_size.to_point().downscale(2.);
            let anchor = LockedAnchor {
                content: self.viewport_transform().apply_inverse(center),
                display: center,
            };
            *self = Self::LockedGesture {
                view,
                gesture: LockedGesturePayload {
                    start_level: level,
                    current_level: level,
                    focal: LockedGestureFocal::Anchored(anchor),
                },
            };
        } else {
            let anchor = FlexibleAnchor {
                content: anchor,
                display: self.viewport_transform().apply(anchor),
            };
            *self = Self::Gesture {
                view,
                gesture: GesturePayload {
                    start_level: level,
                    current_level: level,
                    focal: GestureFocal::Anchored(anchor),
                },
            };
        }
    }

    /// Applies a gesture update's cumulative scale to the zoom level.
    ///
    /// `scale` is relative to the gesture begin: the new level is
    /// `start_level * scale`, clamped to `1..=max_zoom` and snapped to
    /// exactly 1 within [`ZOOM_SNAP_TO_ONE_EPSILON`]. The intent level tracks
    /// the displayed level.
    ///
    /// Non-finite or non-positive scales are ignored without disturbing the
    /// gesture. Does nothing unless a gesture is in progress.
    ///
    /// Returns `true` if the displayed state changed.
    pub fn update_gesture(&mut self, scale: f64, max_zoom: f64) -> bool {
        let (start_level, current_level) = match self {
            Self::Gesture { gesture, .. } => (gesture.start_level, &mut gesture.current_level),
            Self::LockedGesture { gesture, .. } => {
                (gesture.start_level, &mut gesture.current_level)
            }
            _ => return false,
        };

        if !scale.is_finite() || scale <= 0. {
            trace!("ignoring invalid pinch scale {scale}");
            return false;
        }

        let mut level = (start_level * scale).clamp(1., max_zoom);
        if level <= 1. + ZOOM_SNAP_TO_ONE_EPSILON {
            level = 1.;
        }

        if *current_level == level {
            return false;
        }

        *current_level = level;
        true
    }

    /// Commits the current gesture state and ends the gesture.
    ///
    /// The displayed level and focal point become the committed state; a
    /// cancelled gesture commits the same way rather than snapping back to
    /// the begin state. Does nothing unless a gesture is in progress.
    pub fn end_gesture(&mut self) {
        if !self.is_gesturing() {
            return;
        }

        let focal = self.focal();
        let locked = self.is_locked();
        let mut view = *self.view();
        view.level = self.target_level();
        view.focal = focal;

        *self = if locked {
            Self::Locked(view)
        } else {
            Self::Idle(view)
        };
    }

    /// Adjusts the focal point after the output size changed.
    ///
    /// The focal point is preserved if it remains valid for the new size and
    /// clamped to the nearest allowed point otherwise. At `level == 1` it is
    /// reset to the new output center. An in-progress transition is preserved:
    /// its derived focal point is clamped to the new size on access.
    ///
    /// # Panics
    ///
    /// Panics if `view_size` is not finite or has a negative component.
    pub fn update_view_size(&mut self, view_size: Size<f64, Logical>) {
        assert!(view_size.w.is_finite() && view_size.h.is_finite());
        assert!(view_size.w >= 0. && view_size.h >= 0.);

        // A follow owns the focal point: materialize its current displayed
        // value so the resize clamp applies to the real camera position.
        self.commit_follow_focal();

        let view = self.view_mut();
        view.view_size = view_size;

        if self.level() == 1. {
            self.view_mut().focal = view_size.to_point().downscale(2.);
        } else {
            let focal = Self::clamp_focal(self.view().focal, view_size);
            self.view_mut().focal = focal;
        }
    }

    /// Commits a finished transition, if any.
    ///
    /// Called from the monitor's `advance_animations`. When the transition
    /// first reaches its target, the exact target level and the final
    /// clamped focal point are committed and the transition is dropped. A
    /// combined drift that outlives its command continues seamlessly as a
    /// resting follow.
    pub fn advance_animations(&mut self) {
        match self {
            Self::Zooming { zooming, .. } => match zooming {
                ZoomingState::Command(c) if c.animation().is_clamped_done() => {
                    let focal = self.focal();
                    let Self::Zooming { mut view, zooming } = self.take() else {
                        unreachable!();
                    };
                    let ZoomingState::Command(c) = zooming else {
                        unreachable!();
                    };
                    view.level = c.target();
                    view.focal = focal;
                    *self = Self::Idle(view);
                }
                ZoomingState::Restore(r) if r.animation.is_clamped_done() => {
                    let focal = self.focal();
                    let Self::Zooming { mut view, zooming } = self.take() else {
                        unreachable!();
                    };
                    let ZoomingState::Restore(r) = zooming else {
                        unreachable!();
                    };
                    view.level = r.to_level;
                    view.focal = focal;
                    *self = Self::Idle(view);
                }
                _ => (),
            },
            Self::ZoomingFollow { zooming, .. } => {
                if !zooming.animation().is_clamped_done() {
                    return;
                }
                // The command completed while the drift was still active:
                // the drift continues as a resting follow towards the same
                // deadzone destination, keeping its own step timestamp so no
                // stale time is consumed.
                let focal = self.focal();
                let Self::ZoomingFollow {
                    mut view,
                    zooming,
                    follow,
                } = self.take()
                else {
                    unreachable!();
                };
                view.level = zooming.target();
                view.focal = focal;
                let to_focal = zooming.anchor().map_or(focal, |anchor| {
                    Self::anchored_focal(&view, anchor.content, follow.target_display, view.level)
                });
                if to_focal == focal {
                    *self = Self::Idle(view);
                } else {
                    *self = Self::Follow {
                        view,
                        follow: FollowState {
                            to_focal,
                            last_step: follow.last_step,
                        },
                    };
                }
            }
            Self::LockedZooming { zooming, .. } => {
                if !zooming.animation().is_clamped_done() {
                    return;
                }
                let focal = self.focal();
                let Self::LockedZooming { mut view, zooming } = self.take() else {
                    unreachable!();
                };
                view.level = zooming.target();
                view.focal = focal;
                *self = Self::Locked(view);
            }
            // A distance-driven follow completes when the focal point
            // reaches its target; the step caps exactly at it.
            Self::Follow { follow, .. } => {
                let to_focal = follow.to_focal;
                if self.focal() == to_focal {
                    self.commit_follow_focal();
                }
            }
            // A gesture is driven by input events, not the clock; it never
            // completes here.
            _ => (),
        }
    }

    /// Captures the restorable part of the zoom state.
    ///
    /// The snapshot contains the currently displayed `level` and `focal`, and
    /// the user-requested `target_level`. `locked` is deliberately excluded:
    /// it is a user preference, not part of a temporary viewport override.
    pub fn snapshot(&self) -> ZoomSnapshot {
        ZoomSnapshot {
            level: self.level(),
            target_level: self.target_level(),
            focal: self.focal(),
        }
    }

    /// Restores a previously captured [`snapshot()`](Self::snapshot).
    ///
    /// Lands on the same state the animated restore converges to: the saved
    /// `target_level` and `focal`, without starting a transition; a
    /// transition in progress is cancelled. The focal point is clamped to
    /// `view_size` so the viewport stays within the output. `locked` is
    /// preserved.
    ///
    /// The snapshot's `level` is the displayed level at capture time; it is
    /// validated but not restored: a resting state must satisfy
    /// `level == target_level`, and the saved target is the user's intent
    /// the animated restore would have reached.
    ///
    /// # Panics
    ///
    /// Panics if `snapshot.level` or `snapshot.target_level` is not finite or
    /// less than 1, or if `view_size` is not finite or has a negative
    /// component.
    pub fn restore_immediate(&mut self, snapshot: ZoomSnapshot, view_size: Size<f64, Logical>) {
        assert!(snapshot.level.is_finite() && snapshot.level >= 1.);
        assert!(snapshot.target_level.is_finite() && snapshot.target_level >= 1.);
        assert!(view_size.w.is_finite() && view_size.h.is_finite());
        assert!(view_size.w >= 0. && view_size.h >= 0.);

        // A follow owns the focal point: materialize its current displayed
        // value so the camera position is not lost with the transition.
        self.commit_follow_focal();

        let view = ZoomView {
            level: snapshot.target_level,
            focal: Self::clamp_focal(snapshot.focal, view_size),
            view_size,
        };
        *self = if self.is_locked() {
            Self::Locked(view)
        } else {
            Self::Idle(view)
        };
    }

    /// Restores a previously captured [`snapshot()`](Self::snapshot) with an
    /// animated transition.
    ///
    /// Unlike [`set_target_level()`](Self::set_target_level), which keeps an
    /// action anchor in place, this moves the whole viewport back to the
    /// saved state: the level animates in `log2` space towards
    /// `snapshot.target_level` while the focal point interpolates towards
    /// `snapshot.focal`, clamped to the current `view_size`. Restoring to
    /// 1x keeps the focal point at its current position until completion:
    /// at the identity transform the saved focal point is degenerate, so
    /// moving it early would only pan the viewport.
    ///
    /// The restore is a progress animation `0 → 1` using the same config as
    /// regular zoom transitions. The current log-space level velocity is
    /// carried over as `v_log / Δz`; when the level delta is numerically
    /// negligible the progress starts from rest and the restore is
    /// effectively focal-only.
    ///
    /// While locked, the focal point stays fixed and only the level animates
    /// towards the saved target, like a regular locked zoom transition.
    ///
    /// With `config.off` the snapshot is restored immediately, like
    /// [`restore_immediate()`](Self::restore_immediate).
    ///
    /// # Panics
    ///
    /// Panics if `snapshot.level` or `snapshot.target_level` is not finite or
    /// less than 1.
    pub fn restore_animated(
        &mut self,
        snapshot: ZoomSnapshot,
        clock: &Clock,
        config: niri_config::Animation,
    ) {
        assert!(snapshot.level.is_finite() && snapshot.level >= 1.);
        assert!(snapshot.target_level.is_finite() && snapshot.target_level >= 1.);

        // A follow owns the focal point: materialize its current displayed
        // value so the restore starts from the real camera position.
        self.commit_follow_focal();

        if config.off {
            let view_size = self.view().view_size;
            self.restore_immediate(snapshot, view_size);
            return;
        }

        let to_level = snapshot.target_level;
        let to_focal = Self::clamp_focal(snapshot.focal, self.view().view_size);

        if self.is_locked() {
            // The lock is stronger than focal restoration: keep the camera
            // fixed and animate only the level towards the saved target,
            // anchored on the viewport center like a regular locked zoom
            // command.
            let center = self.view().view_size.to_point().downscale(2.);
            let center_content = self.viewport_transform().apply_inverse(center);
            let from_level = self.level();
            let velocity = self.transition_log_velocity();
            let view = *self.view();

            let animation = Animation::new(
                clock.clone(),
                from_level.log2(),
                to_level.log2(),
                velocity,
                config,
            );
            let command = if to_level == 1. {
                LockedCommandState::ToIdentity {
                    payload: LockedIdentityCommand {
                        animation,
                        focal: self.focal(),
                    },
                }
            } else {
                let payload = LockedZoomCommand {
                    animation,
                    anchor: LockedAnchor {
                        content: center_content,
                        display: center,
                    },
                };
                Self::locked_command_by_delta(to_level, from_level, payload)
            };
            *self = Self::LockedZooming {
                view,
                zooming: LockedZoomingState::Command(command),
            };
            return;
        }

        let from_level = self.level();
        let from_focal = self.focal();

        if from_level == to_level && from_focal == to_focal {
            // Already at the destination: commit it, cancelling any in-flight
            // transition.
            *self = Self::Idle(ZoomView {
                level: to_level,
                focal: to_focal,
                view_size: self.view().view_size,
            });
            return;
        }

        // Map the current log-space level velocity onto the progress
        // animation: z = z0 + Δz * p, so v_progress = v_log / Δz.
        let dz = to_level.log2() - from_level.log2();
        let v_progress = if dz.abs() > RESTORE_VELOCITY_MIN_DELTA {
            self.transition_log_velocity() / dz
        } else {
            0.
        };

        *self = Self::Zooming {
            view: *self.view(),
            zooming: ZoomingState::Restore(RestorePayload {
                animation: Animation::new(clock.clone(), 0., 1., v_progress, config),
                from_level,
                to_level,
                from_focal,
                to_focal,
                clock: clock.clone(),
                config,
            }),
        };
    }

    /// The current log-space level velocity of the transition in progress.
    ///
    /// For a regular level command this is the animation velocity directly.
    /// For a restore, the progress velocity is scaled by the level delta.
    /// Returns 0 when the velocity is unavailable (easing curves) or absent.
    fn transition_log_velocity(&self) -> f64 {
        // The restore's progress velocity is scaled by the level delta;
        // every other transition animates `log2(level)` directly.
        let dz = match self {
            Self::Zooming {
                zooming: ZoomingState::Restore(r),
                ..
            } => r.to_level.log2() - r.from_level.log2(),
            _ => 1.,
        };
        self.animation().and_then(|a| a.velocity()).unwrap_or(0.) * dz
    }

    /// Converts an in-progress restore into a regular level command.
    ///
    /// Used when user input takes over the camera mid-restore: the saved
    /// focal destination is abandoned, the level keeps animating towards the
    /// restore target with its current velocity, and the focal point follows
    /// `anchor` from the current displayed position.
    ///
    /// Does nothing unless the current state is a restore.
    fn convert_restore_to_command(&mut self, anchor: FlexibleAnchor) {
        let level = self.level();
        // The displayed focal point is sampled before the restore is taken
        // apart: afterwards `focal()` would report the stale committed value.
        let displayed_focal = self.focal();
        let Self::Zooming {
            view,
            zooming: ZoomingState::Restore(r),
        } = self.take()
        else {
            return;
        };

        let dz = r.to_level.log2() - r.from_level.log2();
        let velocity = if dz.abs() > RESTORE_VELOCITY_MIN_DELTA {
            r.animation.velocity().unwrap_or(0.) * dz
        } else {
            0.
        };
        let animation =
            Animation::new(r.clock, level.log2(), r.to_level.log2(), velocity, r.config);

        let command = if r.to_level == 1. {
            // Zooming out to the identity transform degenerates the anchored
            // focal solve (it divides by `level - 1`), so an anchor would fly
            // to a clamped edge as the level approaches 1. Keep the current
            // focal point fixed instead, like a `ToIdentity` command.
            ZoomCommandState::ToIdentity {
                payload: IdentityCommand {
                    animation,
                    focal: displayed_focal,
                },
            }
        } else {
            let payload = ZoomCommand { animation, anchor };
            Self::command_by_delta(r.to_level, level, payload)
        };

        *self = Self::Zooming {
            view,
            zooming: ZoomingState::Command(command),
        };
    }

    /// The focal point that keeps `content` displayed at `display` for the
    /// given level, clamped to the output.
    ///
    /// Solves `display = focal + (content - focal) * level` for `focal`. At
    /// `level == 1` the transform is the identity and the focal point is
    /// unconstrained, so the committed focal point is kept.
    fn anchored_focal(
        view: &ZoomView,
        content: Point<f64, Logical>,
        display: Point<f64, Logical>,
        level: f64,
    ) -> Point<f64, Logical> {
        let d = level - 1.;
        if d == 0. {
            return Self::clamp_focal(view.focal, view.view_size);
        }

        let focal = Point::from((
            (level * content.x - display.x) / d,
            (level * content.y - display.y) / d,
        ));
        Self::clamp_focal(focal, view.view_size)
    }

    /// Clamps the focal point so that the logical viewport stays within the
    /// output.
    ///
    /// For the uniform-scale transform the viewport is contained in the output
    /// exactly when `0 <= focal <= view_size` per axis. Non-finite components
    /// (from the anchored focal solve near level 1) clamp to the nearest edge.
    fn clamp_focal(
        focal: Point<f64, Logical>,
        view_size: Size<f64, Logical>,
    ) -> Point<f64, Logical> {
        Point::from((
            focal.x.clamp(0., view_size.w),
            focal.y.clamp(0., view_size.h),
        ))
    }
}

/// A restorable snapshot of an [`OutputZoomState`].
///
/// Contains the committed `level`, the user-requested `target_level` and the
/// `focal` point. See [`OutputZoomState::snapshot`] and
/// [`OutputZoomState::restore_immediate`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ZoomSnapshot {
    /// The committed zoom level.
    pub level: f64,
    /// The user-requested zoom level.
    pub target_level: f64,
    /// The fixed point of the viewport transform.
    pub focal: Point<f64, Logical>,
}

#[cfg(test)]
mod tests {

    use approx::assert_abs_diff_eq;
    use proptest::prelude::*;

    use super::*;

    const EPS: f64 = 1e-9;

    /// A clock starting at zero, driven manually via `set_unadjusted`.
    fn test_clock() -> Clock {
        Clock::with_time(std::time::Duration::ZERO)
    }

    fn test_anim_config() -> niri_config::Animation {
        niri_config::Animation {
            off: false,
            kind: niri_config::animations::Kind::Spring(niri_config::animations::SpringParams {
                damping_ratio: 1.,
                stiffness: 800,
                epsilon: 0.0001,
            }),
        }
    }

    /// Advances the clock by `ms` and commits finished transitions.
    fn advance(state: &mut OutputZoomState, clock: &mut Clock, ms: u64) {
        let now = clock.now_unadjusted() + std::time::Duration::from_millis(ms);
        clock.set_unadjusted(now);
        state.advance_animations();
    }

    /// Deadzone tracking with animations off: the focal point is set
    /// immediately, like the pre-follow semantics.
    fn track(state: &mut OutputZoomState, cursor: Point<f64, Logical>, deadzone: f64) -> bool {
        let clock = test_clock();
        let zoom = niri_config::Zoom {
            deadzone_size: deadzone,
            ..Default::default()
        };
        state.update_focal_for_cursor(cursor, zoom, &clock, niri_config::Animation::new_off())
    }

    /// Deadzone follow evaluation with the given animation config.
    fn follow(
        state: &mut OutputZoomState,
        cursor: Point<f64, Logical>,
        deadzone: f64,
        clock: &Clock,
        config: niri_config::Animation,
    ) -> bool {
        let zoom = niri_config::Zoom {
            deadzone_size: deadzone,
            ..Default::default()
        };
        state.update_follow(cursor, zoom, clock, config)
    }

    fn assert_point_eq(a: Point<f64, Logical>, b: Point<f64, Logical>) {
        assert_abs_diff_eq!(a.x, b.x, epsilon = EPS);
        assert_abs_diff_eq!(a.y, b.y, epsilon = EPS);
    }

    fn assert_viewport_within(viewport: Rectangle<f64, Logical>, output: Rectangle<f64, Logical>) {
        assert!(viewport.loc.x >= output.loc.x - EPS);
        assert!(viewport.loc.y >= output.loc.y - EPS);
        assert!(viewport.loc.x + viewport.size.w <= output.loc.x + output.size.w + EPS);
        assert!(viewport.loc.y + viewport.size.h <= output.loc.y + output.size.h + EPS);
    }

    fn assert_finite_point(p: Point<f64, Logical>) {
        assert!(p.x.is_finite() && p.y.is_finite());
    }

    fn assert_finite_rect(r: Rectangle<f64, Logical>) {
        assert_finite_point(r.loc);
        assert!(r.size.w.is_finite() && r.size.h.is_finite());
    }

    /// A state with the given committed level and focal point.
    fn state_at(
        level: f64,
        focal: Point<f64, Logical>,
        view_size: Size<f64, Logical>,
    ) -> OutputZoomState {
        let mut state = OutputZoomState::new(view_size);
        state.set_level_immediate(level, focal);
        assert_point_eq(state.focal(), focal);
        state
    }

    #[test]
    fn output_zoom_state_initial() {
        let view_size = Size::from((1920., 1080.));
        let state = OutputZoomState::new(view_size);

        assert_eq!(state.level(), 1.);
        assert_eq!(state.target_level(), 1.);
        assert!(!state.is_locked());
        assert_point_eq(state.focal(), Point::from((960., 540.)));
    }

    #[test]
    fn output_zoom_state_transform_factor() {
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);

        let t = state.viewport_transform();
        assert_eq!(t.factor(), 1.);
        assert_point_eq(
            t.apply(Point::from((100., 200.))),
            Point::from((100., 200.)),
        );

        state.set_level_immediate(2., Point::from((960., 540.)));
        assert_eq!(state.viewport_transform().factor(), 2.);
    }

    #[test]
    fn output_zoom_state_target_separate_from_level() {
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let clock = test_clock();

        state.set_target_level(2., Point::from((960., 540.)), &clock, test_anim_config());
        assert_eq!(state.level(), 1.);
        assert_eq!(state.target_level(), 2.);
    }

    #[test]
    fn output_zoom_state_viewport_size() {
        let view_size = Size::from((1920., 1080.));
        let output = Rectangle::from_size(view_size);

        for (level, expected) in [
            (1., (1920., 1080.)),
            (1.25, (1536., 864.)),
            (1.5, (1280., 720.)),
            (2., (960., 540.)),
        ] {
            let mut state = OutputZoomState::new(view_size);
            state.set_level_immediate(level, Point::from((960., 540.)));

            let viewport = state.viewport();
            assert_abs_diff_eq!(viewport.size.w, expected.0, epsilon = EPS);
            assert_abs_diff_eq!(viewport.size.h, expected.1, epsilon = EPS);
            assert_viewport_within(viewport, output);
        }
    }

    #[test]
    fn output_zoom_state_deadzone_geometry() {
        let view_size = Size::from((1920., 1080.));

        let dz = OutputZoomState::deadzone_rect(view_size, 0.);
        assert_point_eq(dz.loc, Point::from((960., 540.)));
        assert_abs_diff_eq!(dz.size.w, 0., epsilon = EPS);
        assert_abs_diff_eq!(dz.size.h, 0., epsilon = EPS);

        let dz = OutputZoomState::deadzone_rect(view_size, 0.5);
        assert_point_eq(dz.loc, Point::from((480., 270.)));
        assert_abs_diff_eq!(dz.size.w, 960., epsilon = EPS);
        assert_abs_diff_eq!(dz.size.h, 540., epsilon = EPS);

        let dz = OutputZoomState::deadzone_rect(view_size, 1.);
        assert_point_eq(dz.loc, Point::from((0., 0.)));
        assert_abs_diff_eq!(dz.size.w, 1920., epsilon = EPS);
        assert_abs_diff_eq!(dz.size.h, 1080., epsilon = EPS);
    }

    #[test]
    fn output_zoom_state_cursor_inside_deadzone() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        // Display (960, 540) is the deadzone center.
        let cursor = state
            .viewport_transform()
            .apply_inverse(Point::from((960., 540.)));
        assert!(!track(&mut state, cursor, 0.5));
        assert_point_eq(state.focal(), Point::from((960., 540.)));

        // Display (700, 400) is inside the deadzone but off-center.
        let cursor = state
            .viewport_transform()
            .apply_inverse(Point::from((700., 400.)));
        assert!(!track(&mut state, cursor, 0.5));
        assert_point_eq(state.focal(), Point::from((960., 540.)));
    }

    #[test]
    fn output_zoom_state_cursor_exits_right() {
        let view_size = Size::from((1920., 1080.));
        let output = Rectangle::from_size(view_size);
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        // Display (1700, 540) is right of the deadzone (right edge at 1440).
        let cursor = state
            .viewport_transform()
            .apply_inverse(Point::from((1700., 540.)));
        assert!(track(&mut state, cursor, 0.5));

        // The cursor lands on the deadzone's right edge.
        let display = state.viewport_transform().apply(cursor);
        assert_point_eq(display, Point::from((1440., 540.)));
        assert_viewport_within(state.viewport(), output);
    }

    #[test]
    fn output_zoom_state_cursor_exits_left() {
        let view_size = Size::from((1920., 1080.));
        let output = Rectangle::from_size(view_size);
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        // Display (200, 540) is left of the deadzone (left edge at 480).
        let cursor = state
            .viewport_transform()
            .apply_inverse(Point::from((200., 540.)));
        assert!(track(&mut state, cursor, 0.5));

        let display = state.viewport_transform().apply(cursor);
        assert_point_eq(display, Point::from((480., 540.)));
        assert_viewport_within(state.viewport(), output);
    }

    #[test]
    fn output_zoom_state_cursor_exits_top() {
        let view_size = Size::from((1920., 1080.));
        let output = Rectangle::from_size(view_size);
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        // Display (960, 100) is above the deadzone (top edge at 270).
        let cursor = state
            .viewport_transform()
            .apply_inverse(Point::from((960., 100.)));
        assert!(track(&mut state, cursor, 0.5));

        let display = state.viewport_transform().apply(cursor);
        assert_point_eq(display, Point::from((960., 270.)));
        assert_viewport_within(state.viewport(), output);
    }

    #[test]
    fn output_zoom_state_cursor_exits_bottom() {
        let view_size = Size::from((1920., 1080.));
        let output = Rectangle::from_size(view_size);
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        // Display (960, 1000) is below the deadzone (bottom edge at 810).
        let cursor = state
            .viewport_transform()
            .apply_inverse(Point::from((960., 1000.)));
        assert!(track(&mut state, cursor, 0.5));

        let display = state.viewport_transform().apply(cursor);
        assert_point_eq(display, Point::from((960., 810.)));
        assert_viewport_within(state.viewport(), output);
    }

    #[test]
    fn output_zoom_state_cursor_exits_corner() {
        let view_size = Size::from((1920., 1080.));
        let output = Rectangle::from_size(view_size);
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        // Display (1800, 1000) exits the deadzone on both axes.
        let cursor = state
            .viewport_transform()
            .apply_inverse(Point::from((1800., 1000.)));
        assert!(track(&mut state, cursor, 0.5));

        // The cursor lands on the deadzone's bottom-right corner.
        let display = state.viewport_transform().apply(cursor);
        assert_point_eq(display, Point::from((1440., 810.)));
        assert_viewport_within(state.viewport(), output);
    }

    #[test]
    fn output_zoom_state_viewport_clamp_beats_deadzone() {
        let view_size = Size::from((1920., 1080.));
        let output = Rectangle::from_size(view_size);
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        // Cursor near the left content edge: the deadzone would want focal
        // x = -480, but the viewport cannot leave the output.
        let cursor = Point::from((10., 540.));
        assert!(track(&mut state, cursor, 0.5));

        assert_point_eq(state.focal(), Point::from((0., 540.)));
        assert_viewport_within(state.viewport(), output);

        // The displayed cursor stays left of the deadzone; that is correct.
        let display = state.viewport_transform().apply(cursor);
        assert!(display.x < 480.);
        assert!(display.x >= 0.);
    }

    #[test]
    fn output_zoom_state_deadzone_zero() {
        let view_size = Size::from((1920., 1080.));
        let output = Rectangle::from_size(view_size);
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        // With a zero deadzone the cursor is kept at the output center.
        let cursor = Point::from((1200., 700.));
        assert!(track(&mut state, cursor, 0.));
        let display = state.viewport_transform().apply(cursor);
        assert_point_eq(display, Point::from((960., 540.)));
        assert_viewport_within(state.viewport(), output);

        // At the content edge the viewport clamps and the cursor leaves the
        // center.
        let cursor = Point::from((10., 540.));
        assert!(track(&mut state, cursor, 0.));
        assert_point_eq(state.focal(), Point::from((0., 540.)));
        let display = state.viewport_transform().apply(cursor);
        assert_point_eq(display, Point::from((20., 540.)));
        assert_viewport_within(state.viewport(), output);
    }

    #[test]
    fn output_zoom_state_deadzone_one() {
        let view_size = Size::from((1920., 1080.));
        let output = Rectangle::from_size(view_size);
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        // The deadzone covers the whole output: no tracking while the
        // displayed cursor is inside.
        let cursor = Point::from((1200., 700.));
        assert!(!track(&mut state, cursor, 1.));
        assert_point_eq(state.focal(), Point::from((960., 540.)));

        // Once the cursor would display outside the output, the focal point
        // pushes it back to the edge.
        let cursor = Point::from((1900., 540.));
        assert!(track(&mut state, cursor, 1.));
        let display = state.viewport_transform().apply(cursor);
        assert_point_eq(display, Point::from((1920., 540.)));
        assert_viewport_within(state.viewport(), output);
    }

    #[test]
    fn output_zoom_state_level_one_no_tracking() {
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);

        for deadzone in [0., 0.5, 1.] {
            assert!(!track(&mut state, Point::from((10., 10.)), deadzone));
            assert_point_eq(state.focal(), Point::from((960., 540.)));
        }

        assert_finite_point(state.focal());
        assert_finite_rect(state.viewport());
    }

    #[test]
    fn output_zoom_state_locked_stops_tracking() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);
        let pointer = Point::from((1500., 540.));

        state.set_locked(true, pointer);
        assert!(state.is_locked());
        assert!(matches!(state, OutputZoomState::Locked(_)));

        // This cursor would move the focal point if not locked.
        assert!(!track(&mut state, pointer, 0.5));
        assert_point_eq(state.focal(), Point::from((960., 540.)));

        state.toggle_locked(pointer);
        assert!(!state.is_locked());
        assert!(matches!(state, OutputZoomState::Idle(_)));
        assert!(track(&mut state, pointer, 0.5));
    }

    #[test]
    fn output_zoom_state_immediate_level_keeps_anchor() {
        let view_size = Size::from((1920., 1080.));
        let output = Rectangle::from_size(view_size);
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        let anchor = Point::from((1200., 700.));
        let screen_anchor = state.viewport_transform().apply(anchor);

        state.set_level_immediate(3., anchor);
        assert_eq!(state.level(), 3.);
        assert_eq!(state.target_level(), 3.);

        let new_display = state.viewport_transform().apply(anchor);
        assert_point_eq(new_display, screen_anchor);
        assert_viewport_within(state.viewport(), output);
    }

    #[test]
    fn output_zoom_state_immediate_zoom_from_identity() {
        let view_size = Size::from((1920., 1080.));
        let output = Rectangle::from_size(view_size);
        let mut state = OutputZoomState::new(view_size);

        // At level 1 displayed positions equal content positions.
        let anchor = Point::from((1200., 700.));
        state.set_level_immediate(2., anchor);

        let display = state.viewport_transform().apply(anchor);
        assert_point_eq(display, anchor);
        assert_viewport_within(state.viewport(), output);
    }

    #[test]
    fn output_zoom_state_immediate_reset_to_one() {
        let view_size = Size::from((1920., 1080.));
        let output = Rectangle::from_size(view_size);
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        state.set_level_immediate(1., Point::from((1200., 700.)));
        assert_eq!(state.level(), 1.);
        assert_eq!(state.target_level(), 1.);

        assert_finite_point(state.focal());
        assert_viewport_within(state.viewport(), output);
    }

    #[test]
    fn output_zoom_state_end_session() {
        let view_size = Size::from((1920., 1080.));
        let output = Rectangle::from_size(view_size);
        let clock = test_clock();
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        // A session in every transient state at once: locked, mid level
        // animation with a deadzone drift pending.
        state.set_locked(true, Point::from((960., 540.)));
        state.set_target_level(4., Point::from((100., 100.)), &clock, test_anim_config());
        assert!(state.is_animating());

        state.end_session();

        assert_eq!(state.level(), 1.);
        assert_eq!(state.target_level(), 1.);
        assert!(!state.is_locked());
        assert!(!state.is_animating());
        assert!(!state.is_gesturing());
        assert_eq!(state.viewport_transform().factor(), 1.);
        assert_finite_point(state.focal());
        assert_viewport_within(state.viewport(), output);

        // The session is gone: a follow cannot be committed or suspended,
        // and ending a gesture is a no-op.
        assert!(!state.commit_follow_focal());
        assert!(!state.suspend_follow());
        state.end_gesture();
        assert_eq!(state.level(), 1.);

        // Idempotent: ending an already-ended session changes nothing.
        state.end_session();
        assert_eq!(state.level(), 1.);
        assert_eq!(state.viewport_transform().factor(), 1.);
    }

    #[test]
    fn output_zoom_state_end_session_during_follow() {
        let view_size = Size::from((1920., 1080.));
        let clock = test_clock();
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        // Park the cursor outside the deadzone so a follow owns the focal
        // point.
        follow(
            &mut state,
            Point::from((500., 300.)),
            0.33,
            &clock,
            test_anim_config(),
        );
        assert!(matches!(state, OutputZoomState::Follow { .. }));

        state.end_session();

        assert_eq!(state.level(), 1.);
        assert!(matches!(state, OutputZoomState::Idle(_)));
        assert!(!state.is_locked());
    }

    #[test]
    fn output_zoom_state_focal_clamp() {
        let view_size = Size::from((1920., 1080.));
        let output = Rectangle::from_size(view_size);

        // Zooming out with a focal point at an output edge pushes the new
        // focal point past the opposite edge; it must clamp.
        let cases = [
            // (start focal, anchor, expected focal)
            (
                Point::from((0., 540.)),
                Point::from((960., 540.)),
                Point::from((0., 540.)),
            ),
            (
                Point::from((1920., 540.)),
                Point::from((960., 540.)),
                Point::from((1920., 540.)),
            ),
            (
                Point::from((960., 0.)),
                Point::from((960., 540.)),
                Point::from((960., 0.)),
            ),
            (
                Point::from((960., 1080.)),
                Point::from((960., 540.)),
                Point::from((960., 1080.)),
            ),
            (
                Point::from((0., 0.)),
                Point::from((960., 540.)),
                Point::from((0., 0.)),
            ),
            (
                Point::from((1920., 1080.)),
                Point::from((960., 540.)),
                Point::from((1920., 1080.)),
            ),
        ];

        for (focal, anchor, expected) in cases {
            let mut state = state_at(3., focal, view_size);
            state.set_level_immediate(2., anchor);
            assert_point_eq(state.focal(), expected);
            assert_viewport_within(state.viewport(), output);
        }
    }

    #[test]
    fn output_zoom_state_view_size_change() {
        let view_size = Size::from((1920., 1080.));
        let smaller = Size::from((1280., 720.));
        let output = Rectangle::from_size(smaller);

        // A focal point valid for the old size can leave the new output.
        let mut state = state_at(2., Point::from((1500., 900.)), view_size);
        state.update_view_size(smaller);
        assert_point_eq(state.focal(), Point::from((1280., 720.)));
        assert_viewport_within(state.viewport(), output);

        // A focal point still valid is preserved.
        let mut state = state_at(2., Point::from((640., 360.)), view_size);
        state.update_view_size(smaller);
        assert_point_eq(state.focal(), Point::from((640., 360.)));
        assert_viewport_within(state.viewport(), output);
    }

    #[test]
    fn output_zoom_state_view_size_change_at_level_one() {
        let view_size = Size::from((1920., 1080.));
        let smaller = Size::from((1280., 720.));

        let mut state = OutputZoomState::new(view_size);
        // Move the focal point off-center, then reset to 1x.
        state.set_level_immediate(2., Point::from((1200., 700.)));
        state.set_level_immediate(1., Point::from((1200., 700.)));

        state.update_view_size(smaller);
        assert_point_eq(state.focal(), Point::from((640., 360.)));
    }

    #[test]
    fn output_zoom_state_independent_instances() {
        let view_size = Size::from((1920., 1080.));
        let mut a = OutputZoomState::new(view_size);
        let b = OutputZoomState::new(view_size);

        a.set_level_immediate(2., Point::from((960., 540.)));
        a.set_locked(true, Point::from((960., 540.)));

        assert_eq!(b.level(), 1.);
        assert_eq!(b.target_level(), 1.);
        assert!(!b.is_locked());
        assert_point_eq(b.focal(), Point::from((960., 540.)));
    }

    #[test]
    fn output_zoom_state_invalid_levels_panic() {
        let view_size = Size::from((1920., 1080.));

        for level in [0., 0.5, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut state = OutputZoomState::new(view_size);
            let clock = test_clock();
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                state.set_target_level(level, Point::from((0., 0.)), &clock, test_anim_config())
            }))
            .is_err());

            let mut state = OutputZoomState::new(view_size);
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                state.set_level_immediate(level, Point::from((0., 0.)))
            }))
            .is_err());
        }
    }

    // --- Animated transitions ---

    fn easing_config(duration_ms: u32) -> niri_config::Animation {
        niri_config::Animation {
            off: false,
            kind: niri_config::animations::Kind::Easing(niri_config::animations::EasingParams {
                duration_ms,
                curve: niri_config::animations::Curve::Linear,
            }),
        }
    }

    /// An underdamped spring config that would overshoot without clamping.
    fn underdamped_config() -> niri_config::Animation {
        niri_config::Animation {
            off: false,
            kind: niri_config::animations::Kind::Spring(niri_config::animations::SpringParams {
                damping_ratio: 0.5,
                stiffness: 800,
                epsilon: 0.0001,
            }),
        }
    }

    /// The animation inside the current transition, if any.
    fn transition_animation(state: &OutputZoomState) -> Option<&crate::animation::Animation> {
        state.animation()
    }

    /// The FSM state name, for assertion messages.
    fn state_name(state: &OutputZoomState) -> &'static str {
        match state {
            OutputZoomState::Idle(_) => "Idle",
            OutputZoomState::Follow { .. } => "Follow",
            OutputZoomState::Zooming {
                zooming: ZoomingState::Command(_),
                ..
            } => "Zooming(Command)",
            OutputZoomState::Zooming {
                zooming: ZoomingState::Restore(_),
                ..
            } => "Zooming(Restore)",
            OutputZoomState::ZoomingFollow { .. } => "ZoomingFollow",
            OutputZoomState::Gesture { .. } => "Gesture",
            OutputZoomState::Locked(_) => "Locked",
            OutputZoomState::LockedZooming { .. } => "LockedZooming",
            OutputZoomState::LockedGesture { .. } => "LockedGesture",
        }
    }

    #[test]
    fn transition_zoom_in_smooth() {
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let anchor = Point::from((960., 540.));

        state.set_target_level(2., anchor, &clock, test_anim_config());
        assert!(state.is_animating());
        assert_eq!(state.level(), 1.);
        assert_eq!(state.target_level(), 2.);

        advance(&mut state, &mut clock, 50);
        let mid = state.level();
        assert!(mid > 1. && mid < 2., "intermediate level: {mid}");
        assert!(state.is_animating());

        advance(&mut state, &mut clock, 5000);
        assert!(!state.is_animating());
        assert_eq!(state.level(), 2.);
        assert_eq!(state.target_level(), 2.);
    }

    #[test]
    fn transition_zoom_out_exact_identity() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);
        let mut clock = test_clock();

        state.set_target_level(1., Point::from((960., 540.)), &clock, test_anim_config());

        advance(&mut state, &mut clock, 50);
        let mid = state.level();
        assert!(mid > 1. && mid < 2., "intermediate level: {mid}");

        advance(&mut state, &mut clock, 5000);
        assert_eq!(state.level(), 1.);
        assert_eq!(state.target_level(), 1.);

        // The transform is exactly the identity.
        let t = state.viewport_transform();
        assert_eq!(t.factor(), 1.);
        assert_point_eq(
            t.apply(Point::from((123., 456.))),
            Point::from((123., 456.)),
        );
    }

    #[test]
    fn transition_config_off_is_immediate() {
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let clock = test_clock();

        state.set_target_level(
            2.,
            Point::from((960., 540.)),
            &clock,
            niri_config::Animation::new_off(),
        );

        assert!(!state.is_animating());
        assert_eq!(state.level(), 2.);
        assert_eq!(state.target_level(), 2.);
    }

    #[test]
    fn transition_retarget_same_direction_no_jump() {
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let anchor = Point::from((960., 540.));

        state.set_target_level(2., anchor, &clock, test_anim_config());
        advance(&mut state, &mut clock, 50);
        let before = state.level();
        assert!(before > 1. && before < 2.);

        state.set_target_level(3., anchor, &clock, test_anim_config());
        assert_eq!(state.target_level(), 3.);
        // The new transition starts from the displayed level: no jump.
        assert_abs_diff_eq!(state.level(), before, epsilon = EPS);
        assert!(state.is_animating());

        advance(&mut state, &mut clock, 5000);
        assert_eq!(state.level(), 3.);
    }

    #[test]
    fn transition_retarget_reverse_no_jump() {
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let anchor = Point::from((960., 540.));

        state.set_target_level(3., anchor, &clock, test_anim_config());
        advance(&mut state, &mut clock, 50);
        let before = state.level();
        assert!(before > 1. && before < 3.);

        state.set_target_level(1., anchor, &clock, test_anim_config());
        assert_eq!(state.target_level(), 1.);
        assert_abs_diff_eq!(state.level(), before, epsilon = EPS);

        advance(&mut state, &mut clock, 5000);
        assert_eq!(state.level(), 1.);
    }

    #[test]
    fn transition_spring_retarget_preserves_velocity() {
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let anchor = Point::from((960., 540.));

        state.set_target_level(2., anchor, &clock, test_anim_config());
        advance(&mut state, &mut clock, 50);

        let v_before = transition_animation(&state).unwrap().velocity().unwrap();
        assert!(v_before > 0., "zooming in must have positive z-velocity");

        state.set_target_level(3., anchor, &clock, test_anim_config());
        let v_after = transition_animation(&state).unwrap().velocity().unwrap();

        // The retargeted spring keeps the current velocity.
        assert_abs_diff_eq!(v_after, v_before, epsilon = 1e-6);
    }

    #[test]
    fn transition_spring_reversal_keeps_velocity_sign() {
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let anchor = Point::from((960., 540.));

        state.set_target_level(3., anchor, &clock, test_anim_config());
        advance(&mut state, &mut clock, 50);
        let before = state.level();
        let v_before = transition_animation(&state).unwrap().velocity().unwrap();
        assert!(v_before > 0.);

        // Reverse: the new spring starts with the still-positive velocity and
        // physically turns around.
        state.set_target_level(1., anchor, &clock, test_anim_config());
        let v_after = transition_animation(&state).unwrap().velocity().unwrap();
        assert_abs_diff_eq!(v_after, v_before, epsilon = 1e-6);
        assert_abs_diff_eq!(state.level(), before, epsilon = EPS);

        // The spring briefly continues up before turning around.
        advance(&mut state, &mut clock, 10);
        assert!(state.level() >= before - EPS);

        advance(&mut state, &mut clock, 5000);
        assert_eq!(state.level(), 1.);
    }

    #[test]
    fn transition_easing_retarget_position_continuous() {
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let anchor = Point::from((960., 540.));

        state.set_target_level(2., anchor, &clock, easing_config(1000));
        advance(&mut state, &mut clock, 100);
        let before = state.level();
        assert!(before > 1. && before < 2.);

        // Easing has no analytical velocity; the retarget resets it to zero
        // but must not move the displayed level.
        state.set_target_level(3., anchor, &clock, easing_config(1000));
        assert_abs_diff_eq!(state.level(), before, epsilon = EPS);
        assert_eq!(
            transition_animation(&state).unwrap().velocity(),
            None,
            "easing velocity is unsupported"
        );

        advance(&mut state, &mut clock, 2000);
        assert_eq!(state.level(), 3.);
    }

    #[test]
    fn transition_log_space_equal_steps() {
        let view_size = Size::from((1920., 1080.));
        let anchor = Point::from((960., 540.));

        let mut a = OutputZoomState::new(view_size);
        let clock_a = test_clock();
        a.set_target_level(2., anchor, &clock_a, test_anim_config());

        let mut b = state_at(2., Point::from((960., 540.)), view_size);
        let clock_b = test_clock();
        b.set_target_level(4., anchor, &clock_b, test_anim_config());

        let anim_a = transition_animation(&a).unwrap();
        let anim_b = transition_animation(&b).unwrap();
        assert_abs_diff_eq!(
            anim_a.to() - anim_a.from(),
            anim_b.to() - anim_b.from(),
            epsilon = EPS
        );
    }

    #[test]
    fn transition_no_overshoot_underdamped() {
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let anchor = Point::from((960., 540.));

        state.set_target_level(2., anchor, &clock, underdamped_config());

        for _ in 0..200 {
            advance(&mut state, &mut clock, 10);
            let level = state.level();
            assert!(level >= 1., "level below start: {level}");
            assert!(level <= 2., "level overshot target: {level}");
        }
        assert_eq!(state.level(), 2.);
    }

    #[test]
    fn transition_anchor_stays_put() {
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let anchor = Point::from((960., 540.));

        state.set_target_level(2., anchor, &clock, test_anim_config());
        let display_at_start = state.viewport_transform().apply(anchor);

        for _ in 0..10 {
            advance(&mut state, &mut clock, 20);
            let display = state.viewport_transform().apply(anchor);
            assert_point_eq(display, display_at_start);
        }
    }

    #[test]
    fn transition_to_one_is_continuous_and_exact() {
        let view_size = Size::from((1920., 1080.));
        let output = Rectangle::from_size(view_size);
        // Focal away from the anchor so that S != A and the anchored focal
        // solve diverges as the level approaches 1.
        let mut state = state_at(2., Point::from((0., 0.)), view_size);
        let mut clock = test_clock();
        let anchor = Point::from((960., 540.));

        state.set_target_level(1., anchor, &clock, test_anim_config());

        let mut prev_level = state.level();
        let mut prev_anchor_display = state.viewport_transform().apply(anchor);
        for _ in 0..200 {
            advance(&mut state, &mut clock, 5);
            let level = state.level();
            let focal = state.focal();
            let anchor_display = state.viewport_transform().apply(anchor);

            assert_finite_point(focal);
            assert_finite_point(anchor_display);
            assert!(level <= prev_level + EPS, "level must decrease: {level}");
            assert_viewport_within(state.viewport(), output);

            // The displayed anchor position may only move as much as the
            // level change explains: display = focal + (anchor - focal) * L
            // with focal inside the output bounds.
            let jump = (anchor_display.x - prev_anchor_display.x)
                .abs()
                .max((anchor_display.y - prev_anchor_display.y).abs());
            let max_jump = (prev_level - level).abs() * 1920. + 1.;
            assert!(
                jump <= max_jump,
                "anchor display jumped by {jump} for level delta {}",
                prev_level - level
            );

            prev_level = level;
            prev_anchor_display = anchor_display;
        }

        assert_eq!(state.level(), 1.);
        assert_eq!(state.viewport_transform().factor(), 1.);
    }

    #[test]
    fn transition_boundary_anchor_clamps_focal() {
        let view_size = Size::from((1920., 1080.));
        let output = Rectangle::from_size(view_size);
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let anchor = Point::from((10., 10.));

        state.set_target_level(4., anchor, &clock, test_anim_config());

        let mut prev_level = state.level();
        for _ in 0..100 {
            advance(&mut state, &mut clock, 10);
            let level = state.level();
            assert_finite_point(state.focal());
            assert!(level >= prev_level - EPS);
            assert_viewport_within(state.viewport(), output);
            prev_level = level;
        }
        assert_eq!(state.level(), 4.);
        assert_viewport_within(state.viewport(), output);
    }

    #[test]
    fn transition_deadzone_drifts_during_level_animation() {
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let anchor = Point::from((960., 540.));

        state.set_target_level(2., anchor, &clock, test_anim_config());
        advance(&mut state, &mut clock, 50);
        let level = state.level();

        // A cursor at the content corner is far outside the deadzone: the
        // level animation keeps owning the viewport, but tracking retargets
        // the anchor's display position so the camera drifts during the
        // animation. With the instant tracking config the drift lands
        // immediately.
        assert!(track(&mut state, Point::from((0., 0.)), 0.));
        assert!(state.is_animating());
        assert_eq!(state.level(), level);
        assert_point_eq(state.focal(), Point::from((0., 0.)));

        // Once the level animation completes the camera is already at the
        // deadzone edge: no follow phase remains.
        advance(&mut state, &mut clock, 5000);
        assert_eq!(state.level(), 2.);
        assert!(!track(&mut state, Point::from((0., 0.)), 0.));
        assert_point_eq(state.focal(), Point::from((0., 0.)));
    }

    #[test]
    fn transition_deadzone_drift_completes_with_animation() {
        let view_size = Size::from((1920., 1080.));
        let output = Rectangle::from_size(view_size);
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let anchor = Point::from((960., 540.));

        state.set_target_level(2., anchor, &clock, test_anim_config());
        advance(&mut state, &mut clock, 50);

        // Tracking during the level animation retargets the anchor drift.
        assert!(track(&mut state, Point::from((0., 0.)), 0.));

        // Once the animation completes the camera is already at the deadzone
        // edge: no separate follow phase runs.
        advance(&mut state, &mut clock, 5000);
        assert_eq!(state.level(), 2.);
        assert!(!state.is_animating());
        assert!(!track(&mut state, Point::from((0., 0.)), 0.));
        assert_point_eq(state.focal(), Point::from((0., 0.)));
        assert_viewport_within(state.viewport(), output);
    }

    /// The displayed displacement of the cursor over one follow step.
    fn follow_display_step(
        state: &mut OutputZoomState,
        cursor: Point<f64, Logical>,
        deadzone: f64,
        clock: &mut Clock,
        ms: u64,
    ) -> f64 {
        let before = state.viewport_transform().apply(cursor);
        advance(state, clock, ms);
        let zoom = niri_config::Zoom {
            deadzone_size: deadzone,
            ..Default::default()
        };
        state.update_follow(cursor, zoom, clock, test_anim_config());
        let after = state.viewport_transform().apply(cursor);
        let d = after - before;
        (d.x * d.x + d.y * d.y).sqrt()
    }

    #[test]
    fn follow_diagonal_speed_matches_axis() {
        let view_size = Size::from((1080., 1080.));

        // deadzone 0.5 on a square output: deadzone (270..810), available
        // space 270 per side. A 135px overshoot is t = 0.5 on each axis.
        // At level 2 with focal (540, 540): cursor = (display + 540) / 2.
        // Horizontal: display (945, 540) -> cursor (742.5, 540).
        // Diagonal: display (945, 945) -> cursor (742.5, 742.5).
        let horizontal = Point::from((742.5, 540.));
        let diagonal = Point::from((742.5, 742.5));

        let mut state_h = state_at(2., Point::from((540., 540.)), view_size);
        let mut state_d = state_at(2., Point::from((540., 540.)), view_size);

        // Start the follows.
        let clock0 = test_clock();
        let zoom = niri_config::Zoom {
            deadzone_size: 0.5,
            ..Default::default()
        };
        state_h.update_follow(horizontal, zoom, &clock0, test_anim_config());
        state_d.update_follow(diagonal, zoom, &clock0, test_anim_config());

        // Each state gets its own clock so both measure the same 16ms step.
        let mut clock_h = test_clock();
        let mut clock_d = test_clock();
        let step_h = follow_display_step(&mut state_h, horizontal, 0.5, &mut clock_h, 16);
        let step_d = follow_display_step(&mut state_d, diagonal, 0.5, &mut clock_d, 16);

        // The distance-driven model gives the same speed magnitude for the
        // same relative depth regardless of direction. The old per-axis
        // springs moved the diagonal case ~sqrt(2) faster.
        assert_abs_diff_eq!(step_d, step_h, epsilon = 1e-6);
    }

    #[test]
    fn transition_locked_keeps_viewport_center_fixed() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((500., 400.)), view_size);
        state.set_locked(true, Point::from((960., 540.)));
        let mut clock = test_clock();

        // The content point at the viewport center: it must stay fixed while
        // the level animates, so the focal point moves.
        let center = view_size.to_point().downscale(2.);
        let center_content = state.viewport_transform().apply_inverse(center);

        state.set_target_level(4., Point::from((960., 540.)), &clock, test_anim_config());
        assert!(matches!(state, OutputZoomState::LockedZooming { .. }));

        for _ in 0..20 {
            advance(&mut state, &mut clock, 10);
            assert_point_eq(state.viewport_transform().apply(center_content), center);
        }
        advance(&mut state, &mut clock, 5000);
        assert_eq!(state.level(), 4.);
        assert_point_eq(state.viewport_transform().apply(center_content), center);
    }

    #[test]
    fn transition_unlock_mid_animation_no_jump() {
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let anchor = Point::from((960., 540.));
        let pointer = Point::from((1200., 700.));

        state.zoom_in(3., anchor, &clock, test_anim_config());
        advance(&mut state, &mut clock, 50);
        let mid = state.level();
        assert!(mid > 1. && mid < 3.);

        // Locking keeps the displayed viewport and the target, and
        // re-anchors on the output center.
        let viewport_before = state.viewport();
        state.set_locked(true, pointer);
        assert!(matches!(state, OutputZoomState::LockedZooming { .. }));
        assert_eq!(state.target_level(), 3.);
        assert_abs_diff_eq!(state.level(), mid, epsilon = EPS);
        assert_point_eq(state.viewport().loc, viewport_before.loc);

        let center = view_size.to_point().downscale(2.);
        let center_content = state.viewport_transform().apply_inverse(center);
        advance(&mut state, &mut clock, 20);
        assert_point_eq(state.viewport_transform().apply(center_content), center);

        // Unlocking keeps the displayed viewport and the target, and
        // immediately re-anchors on the pointer.
        let viewport_before = state.viewport();
        state.set_locked(false, pointer);
        assert!(
            matches!(
                state,
                OutputZoomState::Zooming {
                    zooming: ZoomingState::Command(ZoomCommandState::In { target, .. }),
                    ..
                } if target == 3.
            ),
            "expected Zooming(Command(In)), got {}",
            state_name(&state)
        );
        assert_eq!(state.target_level(), 3.);
        assert_abs_diff_eq!(
            state.viewport().size.w,
            viewport_before.size.w,
            epsilon = EPS
        );
        assert_point_eq(state.viewport().loc, viewport_before.loc);

        // The pointer is the anchor: its displayed position stays pinned
        // while the level animates.
        let pointer_display = state.viewport_transform().apply(pointer);
        for _ in 0..5 {
            advance(&mut state, &mut clock, 20);
            assert_point_eq(state.viewport_transform().apply(pointer), pointer_display);
        }

        // Deadzone tracking engages immediately, during the animation.
        assert!(track(&mut state, Point::from((0., 0.)), 0.));
        assert_point_eq(state.focal(), Point::from((0., 0.)));

        advance(&mut state, &mut clock, 5000);
        assert_eq!(state.level(), 3.);
        assert!(matches!(state, OutputZoomState::Idle(_)));
        assert!(!track(&mut state, Point::from((0., 0.)), 0.));
        assert_point_eq(state.focal(), Point::from((0., 0.)));
    }

    #[test]
    fn transition_snapshot_captures_displayed_state() {
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let anchor = Point::from((960., 540.));

        state.set_target_level(2., anchor, &clock, test_anim_config());
        advance(&mut state, &mut clock, 50);

        let snapshot = state.snapshot();
        assert_abs_diff_eq!(snapshot.level, state.level(), epsilon = EPS);
        assert_eq!(snapshot.target_level, 2.);
        assert_point_eq(snapshot.focal, state.focal());
    }

    #[test]
    fn transition_set_level_immediate_cancels() {
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let anchor = Point::from((960., 540.));

        state.set_target_level(2., anchor, &clock, test_anim_config());
        advance(&mut state, &mut clock, 50);

        state.set_level_immediate(4., anchor);
        assert!(!state.is_animating());
        assert_eq!(state.level(), 4.);
        assert_eq!(state.target_level(), 4.);
    }

    #[test]
    fn transition_view_size_change() {
        let view_size = Size::from((1920., 1080.));
        let smaller = Size::from((1280., 720.));
        let output = Rectangle::from_size(smaller);
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let anchor = Point::from((10., 10.));

        state.set_target_level(4., anchor, &clock, test_anim_config());
        advance(&mut state, &mut clock, 50);

        state.update_view_size(smaller);
        assert!(state.is_animating());
        assert_eq!(state.target_level(), 4.);
        assert_finite_point(state.focal());
        assert_viewport_within(state.viewport(), output);

        advance(&mut state, &mut clock, 5000);
        assert_eq!(state.level(), 4.);
        assert_viewport_within(state.viewport(), output);
    }

    #[test]
    fn transition_repeated_targets() {
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let anchor = Point::from((960., 540.));

        for target in [1.2, 1.44, 1.728] {
            state.set_target_level(target, anchor, &clock, test_anim_config());
            assert_eq!(state.target_level(), target);
            advance(&mut state, &mut clock, 10);
        }

        advance(&mut state, &mut clock, 5000);
        assert_abs_diff_eq!(state.level(), 1.728, epsilon = EPS);
    }

    #[test]
    fn transition_complete_instantly_commits() {
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let anchor = Point::from((960., 540.));

        state.set_target_level(2., anchor, &clock, test_anim_config());
        assert!(state.is_animating());

        // The shared clock's instant-completion flag finishes the transition
        // on the next advance without any time passing.
        clock.set_complete_instantly(true);
        state.advance_animations();
        clock.set_complete_instantly(false);

        assert!(!state.is_animating());
        assert_eq!(state.level(), 2.);
        assert_eq!(state.target_level(), 2.);
    }

    // --- Restore transitions ---

    fn snapshot_of(state: &OutputZoomState) -> ZoomSnapshot {
        state.snapshot()
    }

    #[test]
    fn restore_smooth_level_and_focal() {
        let view_size = Size::from((1920., 1080.));
        let output = Rectangle::from_size(view_size);
        let mut state = state_at(2., Point::from((400., 300.)), view_size);
        let mut clock = test_clock();
        let snapshot = snapshot_of(&state);

        // Move the camera away so the restore has to bring the focal back.
        assert!(track(&mut state, Point::from((0., 0.)), 0.));
        assert_point_eq(state.focal(), Point::from((0., 0.)));

        state.restore_animated(snapshot, &clock, test_anim_config());
        assert!(state.is_animating());
        assert_eq!(state.target_level(), 2.);

        advance(&mut state, &mut clock, 50);
        let focal = state.focal();
        assert!(
            focal.x > 0. && focal.x < 400.,
            "intermediate focal: {focal:?}"
        );
        assert!(
            focal.y > 0. && focal.y < 300.,
            "intermediate focal: {focal:?}"
        );

        advance(&mut state, &mut clock, 5000);
        assert!(!state.is_animating());
        assert_eq!(state.level(), 2.);
        assert_point_eq(state.focal(), Point::from((400., 300.)));
        assert_viewport_within(state.viewport(), output);
    }

    #[test]
    fn restore_same_level_focal_only() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((400., 300.)), view_size);
        let mut clock = test_clock();
        let snapshot = snapshot_of(&state);

        // Same level, different focal: the restore is focal-only.
        assert!(track(&mut state, Point::from((0., 0.)), 0.));

        state.restore_animated(snapshot, &clock, test_anim_config());
        assert!(state.is_animating());

        for _ in 0..10 {
            advance(&mut state, &mut clock, 20);
            assert_eq!(state.level(), 2., "level must not move");
            let focal = state.focal();
            assert!(focal.x >= 0. && focal.x <= 400.);
        }

        advance(&mut state, &mut clock, 5000);
        assert_eq!(state.level(), 2.);
        assert_point_eq(state.focal(), Point::from((400., 300.)));
    }

    #[test]
    fn restore_to_one_holds_from_focal() {
        let view_size = Size::from((1920., 1080.));
        let output = Rectangle::from_size(view_size);
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();

        // A saved 1x state whose focal differs from the temporary one.
        let saved_focal = Point::from((400., 300.));
        state.restore_immediate(
            ZoomSnapshot {
                level: 1.,
                target_level: 1.,
                focal: saved_focal,
            },
            view_size,
        );
        let snapshot = snapshot_of(&state);
        assert_eq!(snapshot.target_level, 1.);

        // The temporary zoom hold state: 2x around a different focal point.
        let hold_focal = Point::from((1400., 800.));
        state.set_level_immediate(2., hold_focal);
        assert_point_eq(state.focal(), hold_focal);

        state.restore_animated(snapshot, &clock, test_anim_config());
        assert!(state.is_animating());

        // While the level is still above 1 the focal point must not move
        // towards the saved one: at 1x the focal point is degenerate, so
        // interpolating it early pans the viewport for no visual reason.
        advance(&mut state, &mut clock, 50);
        assert!(state.is_animating());
        let level = state.level();
        assert!(level > 1. && level < 2., "intermediate level: {level}");
        assert_point_eq(state.focal(), hold_focal);

        // The displayed transform is a pure scale-down around the hold
        // focal point: a fixed content point only moves along the scale.
        let content = Point::from((700., 500.));
        let expected = ViewportTransform::new(hold_focal, level).apply(content);
        assert_point_eq(state.viewport_transform().apply(content), expected);

        // On completion the saved focal point is restored exactly; at 1x
        // the transform is the identity, so the focal jump is invisible.
        advance(&mut state, &mut clock, 5000);
        assert!(!state.is_animating());
        assert_eq!(state.level(), 1.);
        assert_point_eq(state.focal(), saved_focal);
        assert_point_eq(state.viewport().loc, output.loc);
    }

    #[test]
    fn restore_maps_log_velocity_to_progress() {
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let anchor = Point::from((960., 540.));

        state.set_target_level(2., anchor, &clock, test_anim_config());
        advance(&mut state, &mut clock, 50);
        let level_before = state.level();
        let v_log = transition_animation(&state).unwrap().velocity().unwrap();
        assert!(v_log > 0.);

        // Restore back to 1 while the zoom-in is still moving: the progress
        // velocity must map the log-space velocity through the level delta.
        let mut snapshot = snapshot_of(&state);
        snapshot.target_level = 1.;
        state.restore_animated(snapshot, &clock, test_anim_config());

        let dz = 1f64.log2() - level_before.log2();
        let expected = v_log / dz;
        let v_progress = transition_animation(&state).unwrap().velocity().unwrap();
        assert_abs_diff_eq!(v_progress, expected, epsilon = 1e-6);

        // Position is continuous.
        assert_abs_diff_eq!(state.level(), level_before, epsilon = EPS);

        // The still-positive velocity briefly continues the zoom-in before
        // the spring turns around.
        advance(&mut state, &mut clock, 10);
        assert!(state.level() >= level_before - EPS);

        advance(&mut state, &mut clock, 5000);
        assert_eq!(state.level(), 1.);
    }

    #[test]
    fn restore_zero_delta_starts_from_rest() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((400., 300.)), view_size);
        let mut clock = test_clock();

        // A level animation with a tiny delta still in flight.
        state.set_target_level(
            2.0005,
            Point::from((960., 540.)),
            &clock,
            test_anim_config(),
        );
        advance(&mut state, &mut clock, 50);

        let mut snapshot = snapshot_of(&state);
        snapshot.target_level = 2.;
        snapshot.focal = Point::from((800., 500.));
        state.restore_animated(snapshot, &clock, test_anim_config());

        // The level delta is numerically negligible: no huge progress
        // velocity, the restore is effectively focal-only.
        let v_progress = transition_animation(&state).unwrap().velocity().unwrap();
        assert_eq!(v_progress, 0.);

        advance(&mut state, &mut clock, 5000);
        assert_eq!(state.level(), 2.);
        assert_point_eq(state.focal(), Point::from((800., 500.)));
    }

    #[test]
    fn restore_deadzone_converts_to_level_animation() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((400., 300.)), view_size);
        let mut clock = test_clock();
        let snapshot = snapshot_of(&state);

        assert!(track(&mut state, Point::from((0., 0.)), 0.));
        // A slow easing restore so that it is still in flight when the
        // deadzone tracking kicks in.
        state.restore_animated(snapshot, &clock, easing_config(1000));
        advance(&mut state, &mut clock, 50);
        let level = state.level();
        assert!(state.is_animating());

        // Deadzone tracking during the restore abandons the saved focal
        // destination: the restore converts to a regular level animation
        // anchored on the cursor, keeping the displayed focal continuous.
        let focal_before = state.focal();
        assert!(track(&mut state, Point::from((0., 0.)), 0.));
        assert_point_eq(state.focal(), focal_before);

        // The transition is now a regular level animation towards the same
        // target; the level did not jump.
        assert!(matches!(
            state,
            OutputZoomState::Zooming {
                zooming: ZoomingState::Command(_),
                ..
            } | OutputZoomState::ZoomingFollow { .. }
        ));
        assert_eq!(state.level(), level);
        assert!(state.is_animating());

        advance(&mut state, &mut clock, 5000);
        assert_eq!(state.level(), 2.);
        // After the level animation completes, the follow evaluation brings
        // the camera to the deadzone edge.
        assert!(track(&mut state, Point::from((0., 0.)), 0.));
        assert_point_eq(state.focal(), Point::from((0., 0.)));
    }

    #[test]
    fn restore_lock_converts_to_center_anchored_command() {
        let view_size = Size::from((1920., 1080.));
        // A focal point near the center so that the center-anchored solve
        // stays within the output bounds down to the restore target.
        let mut state = state_at(3., Point::from((700., 450.)), view_size);
        let mut clock = test_clock();

        // A slow easing restore so that it is still in flight when the lock
        // engages.
        let mut snapshot = snapshot_of(&state);
        snapshot.target_level = 1.5;
        state.restore_animated(snapshot, &clock, easing_config(1000));
        advance(&mut state, &mut clock, 50);
        assert!(state.is_animating());

        // Locking mid-restore converts it to a locked command: the restore
        // target becomes the command target, the displayed viewport does
        // not jump, and the camera re-anchors on the output center instead
        // of freezing the focal point.
        let viewport_before = state.viewport();
        state.set_locked(true, Point::from((960., 540.)));
        assert!(
            matches!(state, OutputZoomState::LockedZooming { .. }),
            "expected LockedZooming, got {}",
            state_name(&state)
        );
        assert_eq!(state.target_level(), 1.5);
        assert_point_eq(state.viewport().loc, viewport_before.loc);
        assert_abs_diff_eq!(
            state.viewport().size.w,
            viewport_before.size.w,
            epsilon = EPS
        );

        // Center anchoring: the content under the output center stays fixed
        // while the level animates, so the focal point moves rather than
        // freezing at the lock position.
        let center = view_size.to_point().downscale(2.);
        let center_content = state.viewport_transform().apply_inverse(center);
        let lock_focal = state.focal();
        let mut focal_moved = false;
        for _ in 0..10 {
            advance(&mut state, &mut clock, 20);
            assert_point_eq(state.viewport_transform().apply(center_content), center);
            focal_moved |= state.focal() != lock_focal;
        }
        assert!(focal_moved, "locked command must re-anchor on the center");

        advance(&mut state, &mut clock, 5000);
        assert_eq!(state.level(), 1.5);
        assert!(matches!(state, OutputZoomState::Locked(_)));
        assert_point_eq(state.viewport_transform().apply(center_content), center);
    }

    #[test]
    fn restore_locked_skips_focal_restore() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(3., Point::from((400., 300.)), view_size);
        let mut clock = test_clock();
        let snapshot = snapshot_of(&state);

        // Move the camera away and lock it: the lock is stronger than the
        // saved focal destination.
        assert!(track(&mut state, Point::from((0., 0.)), 0.));
        state.set_locked(true, Point::from((960., 540.)));
        let fixed = state.focal();

        let mut snapshot = snapshot;
        snapshot.target_level = 2.;
        state.restore_animated(snapshot, &clock, test_anim_config());
        assert!(state.is_animating());

        for _ in 0..10 {
            advance(&mut state, &mut clock, 20);
            assert_point_eq(state.focal(), fixed);
            let level = state.level();
            assert!((2. - EPS..=3. + EPS).contains(&level));
        }
        advance(&mut state, &mut clock, 5000);
        assert_eq!(state.level(), 2.);
        assert_eq!(state.target_level(), 2.);
        assert_point_eq(state.focal(), fixed);
    }

    #[test]
    fn restore_retarget_by_zoom_action() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(3., Point::from((400., 300.)), view_size);
        let mut clock = test_clock();

        let mut snapshot = snapshot_of(&state);
        snapshot.target_level = 1.5;
        state.restore_animated(snapshot, &clock, test_anim_config());
        advance(&mut state, &mut clock, 50);
        let mid = state.level();
        assert!(mid > 1.5 && mid < 3.);

        // A new zoom action retargets from the displayed state: no jump.
        state.set_target_level(4., Point::from((960., 540.)), &clock, test_anim_config());
        assert!(matches!(
            state,
            OutputZoomState::Zooming {
                zooming: ZoomingState::Command(ZoomCommandState::Value { .. }),
                ..
            }
        ));
        assert_eq!(state.target_level(), 4.);
        assert_abs_diff_eq!(state.level(), mid, epsilon = EPS);

        advance(&mut state, &mut clock, 5000);
        assert_eq!(state.level(), 4.);
    }

    #[test]
    fn restore_config_off_is_immediate() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(3., Point::from((400., 300.)), view_size);
        let clock = test_clock();

        let mut snapshot = snapshot_of(&state);
        snapshot.level = 1.5;
        snapshot.target_level = 1.5;
        snapshot.focal = Point::from((800., 500.));
        state.restore_animated(snapshot, &clock, niri_config::Animation::new_off());

        assert!(!state.is_animating());
        assert_eq!(state.level(), 1.5);
        assert_eq!(state.target_level(), 1.5);
        assert_point_eq(state.focal(), Point::from((800., 500.)));
    }

    #[test]
    fn restore_view_size_change_clamps_focal() {
        let view_size = Size::from((1920., 1080.));
        let smaller = Size::from((1280., 720.));
        let output = Rectangle::from_size(smaller);
        let mut state = state_at(3., Point::from((400., 300.)), view_size);
        let mut clock = test_clock();

        let mut snapshot = snapshot_of(&state);
        snapshot.target_level = 2.;
        snapshot.focal = Point::from((1900., 1000.));
        state.restore_animated(snapshot, &clock, test_anim_config());
        advance(&mut state, &mut clock, 50);

        state.update_view_size(smaller);
        assert!(state.is_animating());
        assert_finite_point(state.focal());
        assert_viewport_within(state.viewport(), output);

        advance(&mut state, &mut clock, 5000);
        assert_eq!(state.level(), 2.);
        assert_point_eq(state.focal(), Point::from((1280., 720.)));
        assert_viewport_within(state.viewport(), output);
    }

    #[test]
    fn restore_snapshot_reports_displayed_state() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(3., Point::from((400., 300.)), view_size);
        let mut clock = test_clock();

        let mut snapshot = snapshot_of(&state);
        snapshot.target_level = 1.5;
        snapshot.focal = Point::from((800., 500.));
        state.restore_animated(snapshot, &clock, test_anim_config());
        advance(&mut state, &mut clock, 50);

        // A snapshot taken mid-restore captures the displayed viewport and
        // the restore's target intent, not the from/to endpoints.
        let mid = state.snapshot();
        assert_abs_diff_eq!(mid.level, state.level(), epsilon = EPS);
        assert!(mid.level > 1.5 && mid.level < 3.);
        assert_eq!(mid.target_level, 1.5);
        assert_point_eq(mid.focal, state.focal());
    }

    #[test]
    fn restore_completion_is_exact() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(3., Point::from((400., 300.)), view_size);
        let mut clock = test_clock();

        let mut snapshot = snapshot_of(&state);
        snapshot.target_level = 1.5;
        snapshot.focal = Point::from((800., 500.));
        state.restore_animated(snapshot, &clock, test_anim_config());

        advance(&mut state, &mut clock, 5000);
        assert!(!state.is_animating());
        assert!(matches!(state, OutputZoomState::Idle(_)));
        assert_eq!(state.level(), 1.5);
        assert_eq!(state.target_level(), 1.5);
        assert_point_eq(state.focal(), Point::from((800., 500.)));
    }

    #[test]
    fn restore_no_overshoot_underdamped() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(3., Point::from((400., 300.)), view_size);
        let mut clock = test_clock();

        let mut snapshot = snapshot_of(&state);
        snapshot.target_level = 1.5;
        snapshot.focal = Point::from((800., 500.));
        state.restore_animated(snapshot, &clock, underdamped_config());

        for _ in 0..200 {
            advance(&mut state, &mut clock, 10);
            let level = state.level();
            let focal = state.focal();
            assert!((1.5..=3.).contains(&level), "level out of bounds: {level}");
            assert!(focal.x >= 400. - EPS && focal.x <= 800. + EPS);
            assert!(focal.y >= 300. - EPS && focal.y <= 500. + EPS);
        }
        assert_eq!(state.level(), 1.5);
        assert_point_eq(state.focal(), Point::from((800., 500.)));
    }

    #[test]
    fn gesture_begin_identity() {
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);

        state.begin_gesture(Point::from((960., 540.)));

        assert!(state.is_gesturing());
        assert!(!state.is_animating());
        assert_eq!(state.level(), 1.);
        assert_eq!(state.target_level(), 1.);
    }

    #[test]
    fn gesture_begin_from_animation() {
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();

        state.set_target_level(2., Point::from((960., 540.)), &clock, test_anim_config());
        advance(&mut state, &mut clock, 50);
        let displayed = state.level();
        assert!(displayed > 1. && displayed < 2.);

        // The gesture base is the displayed level, not the animation target.
        state.begin_gesture(Point::from((960., 540.)));

        assert!(state.is_gesturing());
        assert!(matches!(state, OutputZoomState::Gesture { .. }));
        assert_abs_diff_eq!(state.level(), displayed, epsilon = EPS);
        assert_abs_diff_eq!(state.target_level(), displayed, epsilon = EPS);

        // The gesture base is the displayed level: a scale of 1.1 lands on
        // displayed * 1.1, not on anything derived from the abandoned
        // animation target.
        assert!(state.update_gesture(1.1, 10.));
        assert_abs_diff_eq!(state.level(), displayed * 1.1, epsilon = 1e-6);
    }

    #[test]
    fn gesture_begin_from_restore() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(3., Point::from((400., 300.)), view_size);
        let mut clock = test_clock();

        let mut snapshot = snapshot_of(&state);
        snapshot.target_level = 1.5;
        state.restore_animated(snapshot, &clock, test_anim_config());
        advance(&mut state, &mut clock, 50);
        let displayed = state.level();
        assert!(displayed > 1.5 && displayed < 3.);

        // The gesture base is the displayed state; the restore destination
        // is abandoned.
        state.begin_gesture(Point::from((960., 540.)));

        assert!(state.is_gesturing());
        assert_abs_diff_eq!(state.level(), displayed, epsilon = EPS);
        assert_abs_diff_eq!(state.target_level(), displayed, epsilon = EPS);
    }

    #[test]
    fn gesture_scale_up() {
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);

        state.begin_gesture(Point::from((960., 540.)));
        assert!(state.update_gesture(2., 10.));

        assert_eq!(state.level(), 2.);
        assert_eq!(state.target_level(), 2.);
        // Updates never allocate an animation.
        assert!(matches!(state, OutputZoomState::Gesture { .. }));
    }

    #[test]
    fn gesture_scale_down() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        state.begin_gesture(Point::from((960., 540.)));
        assert!(state.update_gesture(0.75, 10.));

        assert_eq!(state.level(), 1.5);
        assert_eq!(state.target_level(), 1.5);
    }

    #[test]
    fn gesture_lower_exact_clamp() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        state.begin_gesture(Point::from((960., 540.)));
        assert!(state.update_gesture(0.4, 10.));

        // Zooming out past 1 snaps to the exact identity, no residue.
        assert_eq!(state.level(), 1.);
        assert_eq!(state.target_level(), 1.);
    }

    #[test]
    fn gesture_max_clamp() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        state.begin_gesture(Point::from((960., 540.)));
        assert!(state.update_gesture(3., 4.));

        assert_eq!(state.level(), 4.);
        assert_eq!(state.target_level(), 4.);
    }

    #[test]
    fn gesture_invalid_scale() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        state.begin_gesture(Point::from((960., 540.)));
        for scale in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0., -1.] {
            assert!(!state.update_gesture(scale, 10.));
            assert!(state.is_gesturing());
            assert_eq!(state.level(), 2.);
            assert_eq!(state.target_level(), 2.);
        }
    }

    #[test]
    fn gesture_anchor_stability() {
        let view_size = Size::from((1920., 1080.));
        let output = Rectangle::from_size(view_size);
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        let anchor = Point::from((700., 400.));
        let display = state.viewport_transform().apply(anchor);
        state.begin_gesture(anchor);

        // The begin anchor keeps its displayed position while the level
        // changes, as long as the output bounds allow it.
        assert!(state.update_gesture(1.5, 10.));
        assert_eq!(state.level(), 3.);
        assert_point_eq(state.viewport_transform().apply(anchor), display);
        assert_viewport_within(state.viewport(), output);
    }

    #[test]
    fn gesture_boundary_clamp() {
        let view_size = Size::from((1920., 1080.));
        let output = Rectangle::from_size(view_size);
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        // An anchor near the output edge cannot keep its displayed position
        // at every level; the focal point stays clamped and finite instead.
        state.begin_gesture(Point::from((10., 10.)));
        assert!(state.update_gesture(2., 10.));

        assert_finite_point(state.focal());
        assert_viewport_within(state.viewport(), output);
    }

    #[test]
    fn gesture_locked_focal() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);
        state.set_locked(true, Point::from((960., 540.)));
        let focal = state.focal();

        state.begin_gesture(Point::from((700., 400.)));
        assert!(state.update_gesture(1.5, 10.));

        assert_eq!(state.level(), 3.);
        // Locked: the camera does not move, only the level changes.
        assert_point_eq(state.focal(), focal);
    }

    #[test]
    fn gesture_lock_mid_gesture() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        state.begin_gesture(Point::from((700., 400.)));
        assert!(state.update_gesture(1.5, 10.));
        let frozen = state.focal();

        // Locking freezes the focal point at its current displayed value.
        state.set_locked(true, Point::from((960., 540.)));
        assert!(matches!(state, OutputZoomState::LockedGesture { .. }));
        assert!(state.update_gesture(0.75, 10.));

        assert_eq!(state.level(), 1.5);
        assert_point_eq(state.focal(), frozen);
    }

    #[test]
    fn gesture_unlock_mid_gesture() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        state.begin_gesture(Point::from((700., 400.)));
        state.set_locked(true, Point::from((960., 540.)));
        assert!(state.update_gesture(1.5, 10.));
        let frozen = state.focal();

        // Unlocking does not jump back to the anchor: the fixed focal point
        // remains until the gesture ends.
        state.set_locked(false, Point::from((960., 540.)));
        assert!(matches!(state, OutputZoomState::Gesture { .. }));
        assert!(state.update_gesture(0.75, 10.));

        assert_point_eq(state.focal(), frozen);
    }

    #[test]
    fn gesture_deadzone_suppressed() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        state.begin_gesture(Point::from((700., 400.)));
        assert!(state.update_gesture(1.25, 10.));
        let focal = state.focal();

        // Pointer motion does not re-pin the gesture anchor.
        assert!(!track(&mut state, Point::from((1800., 900.)), 0.5));
        assert_point_eq(state.focal(), focal);
    }

    #[test]
    fn gesture_snapshot() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        state.begin_gesture(Point::from((700., 400.)));
        assert!(state.update_gesture(1.5, 10.));

        let snapshot = state.snapshot();
        assert_eq!(snapshot.level, 3.);
        assert_eq!(snapshot.target_level, 3.);
        assert_point_eq(snapshot.focal, state.focal());
    }

    #[test]
    fn gesture_end_commits_current() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        state.begin_gesture(Point::from((700., 400.)));
        assert!(state.update_gesture(1.5, 10.));
        let focal = state.focal();

        state.end_gesture();

        assert!(!state.is_gesturing());
        assert!(matches!(state, OutputZoomState::Idle(_)));
        assert_eq!(state.level(), 3.);
        assert_eq!(state.target_level(), 3.);
        assert_point_eq(state.focal(), focal);
    }

    #[test]
    fn gesture_end_at_one_is_exact() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        state.begin_gesture(Point::from((700., 400.)));
        assert!(state.update_gesture(0.4, 10.));
        state.end_gesture();

        assert_eq!(state.level(), 1.);
        assert_eq!(state.target_level(), 1.);
    }

    #[test]
    fn gesture_is_not_animating() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        state.begin_gesture(Point::from((700., 400.)));
        assert!(state.update_gesture(1.5, 10.));

        // A gesture queues its own redraws; it must not keep the output
        // animation flag set between events.
        assert!(!state.is_animating());
        assert!(state.is_gesturing());
    }

    #[test]
    fn gesture_update_view_size() {
        let view_size = Size::from((1920., 1080.));
        let smaller = Size::from((1280., 720.));
        let output = Rectangle::from_size(smaller);
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        state.begin_gesture(Point::from((700., 400.)));
        assert!(state.update_gesture(1.5, 10.));

        // A resize does not disturb the gesture; the focal point stays
        // clamped to the new size.
        state.update_view_size(smaller);

        assert!(state.is_gesturing());
        assert_finite_point(state.focal());
        assert_viewport_within(state.viewport(), output);
    }

    #[test]
    fn follow_does_not_consume_idle_time() {
        // Regression: the follow step must measure only the time the follow
        // itself has been active. A shared evaluation timestamp let a follow
        // started after an idle gap consume the whole gap as its first step,
        // snapping the camera to the deadzone border instead of gliding.
        let view_size = Size::from((1920., 1080.));
        let output = Rectangle::from_size(view_size);
        let mut state = state_at(2., Point::from((960., 540.)), view_size);
        let mut clock = test_clock();
        let zoom = niri_config::Zoom {
            deadzone_size: 0.5,
            ..Default::default()
        };
        let config = test_anim_config();

        // An evaluation inside the deadzone, then a long gap with no
        // evaluations at all (compositor idle, tracking suspended).
        state.update_follow(Point::from((960., 540.)), zoom, &clock, config);
        clock.set_unadjusted(clock.now_unadjusted() + Duration::from_secs(10));

        // The cursor reappears outside the deadzone: the follow starts, but
        // its first step must not consume the idle gap.
        assert!(state.update_follow(Point::from((0., 0.)), zoom, &clock, config));
        assert!(state.is_animating());
        assert_point_eq(state.focal(), Point::from((960., 540.)));

        // The follow then converges normally at the clamped target: the
        // per-frame driver steps it on each evaluation.
        advance(&mut state, &mut clock, 5000);
        assert!(state.update_follow(Point::from((0., 0.)), zoom, &clock, config));
        assert!(!state.is_animating());
        assert_point_eq(state.focal(), Point::from((0., 0.)));
        assert_viewport_within(state.viewport(), output);
    }

    #[test]
    fn follow_large_dt_caps_at_target() {
        // A large frame gap on an active follow must move the camera at most
        // to its clamped target: no overshoot past the deadzone border, no
        // NaN, and the follow terminates there.
        let view_size = Size::from((1920., 1080.));
        let output = Rectangle::from_size(view_size);
        let mut state = state_at(2., Point::from((960., 540.)), view_size);
        let mut clock = test_clock();
        let zoom = niri_config::Zoom {
            deadzone_size: 0.5,
            ..Default::default()
        };
        let config = test_anim_config();
        let cursor = Point::from((100., 540.));

        // Start the follow, then jump the clock far past any real frame.
        assert!(state.update_follow(cursor, zoom, &clock, config));
        clock.set_unadjusted(clock.now_unadjusted() + Duration::from_secs(60));

        assert!(state.update_follow(cursor, zoom, &clock, config));
        assert_finite_point(state.focal());
        assert_viewport_within(state.viewport(), output);

        // The step capped exactly at the target: the follow is done.
        assert!(!state.is_animating());
        let target = state.focal();
        assert!(!state.update_follow(cursor, zoom, &clock, config));
        assert_point_eq(state.focal(), target);
    }

    #[test]
    fn follow_refresh_rate_equivalent() {
        // The follow is time-driven: the same wall time in different frame
        // slices must land within a few percent of each other. The speed is
        // re-evaluated from the live overshoot each step, so finer slices
        // integrate the easing slightly differently — equal results are not
        // expected bit-for-bit.
        let view_size = Size::from((1920., 1080.));
        let zoom = niri_config::Zoom {
            deadzone_size: 0.5,
            ..Default::default()
        };
        let config = test_anim_config();
        let cursor = Point::from((100., 540.));

        let run = |step_ms: u64, steps: usize| {
            let mut state = state_at(2., Point::from((960., 540.)), view_size);
            let mut clock = test_clock();
            state.update_follow(cursor, zoom, &clock, config);
            for _ in 0..steps {
                clock.set_unadjusted(clock.now_unadjusted() + Duration::from_millis(step_ms));
                state.update_follow(cursor, zoom, &clock, config);
            }
            state.focal()
        };

        // 32ms of wall time as 1x32, 2x16 and 4x8.
        let a = run(32, 1);
        let b = run(16, 2);
        let c = run(8, 4);

        assert_abs_diff_eq!(a.x, b.x, epsilon = 2.);
        assert_abs_diff_eq!(a.x, c.x, epsilon = 2.);
        assert_abs_diff_eq!(a.y, b.y, epsilon = 2.);
        assert_abs_diff_eq!(a.y, c.y, epsilon = 2.);
    }

    #[test]
    fn suspended_drift_resumes_without_jump() {
        // Regression: while tracking is suspended the per-frame driver calls
        // the suspend path, which must retire the in-transition deadzone
        // drift. A drift left behind keeps a stale timestamp, so the first
        // evaluation after resume consumes the whole suspension as one step
        // and snaps the camera to the deadzone border.
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let zoom = niri_config::Zoom {
            deadzone_size: 0.5,
            ..Default::default()
        };
        // A long easing keeps the level animation alive across the
        // suspension so the drift marker survives into the resume.
        let config = easing_config(5000);
        let cursor = Point::from((200., 540.));

        state.set_target_level(2., Point::from((960., 540.)), &clock, config);
        advance(&mut state, &mut clock, 50);

        // The cursor leaves the deadzone: the anchor's display position
        // starts drifting during the level animation.
        assert!(state.update_follow(cursor, zoom, &clock, config));
        advance(&mut state, &mut clock, 16);
        assert!(state.update_follow(cursor, zoom, &clock, config));

        // Tracking is suspended mid-animation (session lock, Overview, MRU,
        // screenshot UI, pointer on another output): the driver suspends
        // follow activity without moving the camera.
        state.suspend_follow();

        // The suspension lasts 2s; the level animation keeps running.
        advance(&mut state, &mut clock, 2000);
        assert!(state.is_animating());

        // On resume the drift restarts from the live displayed position: the
        // first evaluation must not move the camera.
        let display_before = state.viewport_transform().apply(cursor);
        assert!(state.update_follow(cursor, zoom, &clock, config));
        let display_after = state.viewport_transform().apply(cursor);
        assert_point_eq(display_after, display_before);
    }

    #[test]
    fn restore_takeover_to_one_keeps_focal() {
        // Regression: pointer tracking taking over a restore towards 1x must
        // keep the focal point fixed, like a regular zoom-out to 1x. An
        // anchored conversion divides by `level - 1`, so the focal point
        // degenerates to a clamped corner and the camera pans away as the
        // level approaches 1.
        let view_size = Size::from((1920., 1080.));
        let output = Rectangle::from_size(view_size);
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let zoom = niri_config::Zoom {
            deadzone_size: 0.5,
            ..Default::default()
        };
        let config = test_anim_config();

        state.set_target_level(3., Point::from((400., 300.)), &clock, config);
        advance(&mut state, &mut clock, 5000);
        assert_eq!(state.level(), 3.);

        let snapshot = ZoomSnapshot {
            level: 1.,
            target_level: 1.,
            focal: view_size.to_point().downscale(2.),
        };
        state.restore_animated(snapshot, &clock, config);
        advance(&mut state, &mut clock, 50);
        assert!(state.level() > 1. && state.level() < 3.);

        // The cursor is outside the deadzone: tracking converts the restore
        // into a level animation.
        let cursor = Point::from((1500., 900.));
        assert!(state.update_focal_for_cursor(cursor, zoom, &clock, config));

        // As the level approaches 1 the focal point must stay near the
        // takeover camera, never degenerating to a clamped corner.
        while state.level() > 1. {
            advance(&mut state, &mut clock, 16);
            let focal = state.focal();
            assert_finite_point(focal);
            assert!(
                focal.x > 0. && focal.y > 0. && focal.x < view_size.w && focal.y < view_size.h,
                "focal {focal:?} degenerated to a clamped edge at level {}",
                state.level()
            );
            assert_viewport_within(state.viewport(), output);
        }
        assert_eq!(state.level(), 1.);
    }

    #[test]
    fn restore_immediate_lands_on_target_level() {
        // Regression: an immediate restore must land on the same state the
        // animated restore converges to — the saved `target_level`, not the
        // mid-flight displayed `level`. Restoring the displayed level while
        // keeping the saved target leaves a resting state where
        // `level != target_level`, which no other path can produce: the IPC
        // target reports a level that was never reached and incremental zoom
        // actions compute from it.
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();

        state.set_target_level(2., Point::from((960., 540.)), &clock, test_anim_config());
        advance(&mut state, &mut clock, 50);
        assert!(state.level() > 1. && state.level() < 2.);

        let snapshot = state.snapshot();
        state.restore_immediate(snapshot, view_size);

        assert_eq!(state.level(), 2.);
        assert_eq!(state.target_level(), 2.);
        assert!(matches!(state, OutputZoomState::Idle(_)));
    }

    #[test]
    fn gesture_through_one_stays_bounded() {
        // A pinch sweeping through 1x must keep the focal point finite and
        // inside the output on every intermediate frame: the anchored solve
        // divides by `level - 1`, so near-1 levels produce huge values that
        // must clamp to the output bounds without a visible camera jump.
        let view_size = Size::from((1920., 1080.));
        let output = Rectangle::from_size(view_size);
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        state.begin_gesture(Point::from((700., 400.)));

        // Sweep the cumulative scale from 1 down through the 1x crossing.
        let mut prev_display = state.viewport_transform().apply(Point::from((700., 400.)));
        for i in 1..=100 {
            let scale = 1. - i as f64 / 200.;
            state.update_gesture(scale, 10.);
            let level = state.level();
            assert!(level >= 1.);
            assert_finite_point(state.focal());
            assert_viewport_within(state.viewport(), output);

            // The displayed anchor moves continuously: no teleport between
            // frames as the level crosses 1.
            let display = state.viewport_transform().apply(Point::from((700., 400.)));
            let d = display - prev_display;
            let jump = (d.x * d.x + d.y * d.y).sqrt();
            assert!(
                jump < 200.,
                "displayed anchor jumped {jump}px at level {level}"
            );
            prev_display = display;
        }

        state.end_gesture();
        assert_eq!(state.level(), 1.);
        assert_eq!(state.target_level(), 1.);
    }

    // --- FSM transition matrix ---
    //
    // The zoom state is a finite state machine over eight variants: `Idle`,
    // `Follow`, `Zooming` (command or restore), `ZoomingFollow`, `Gesture`,
    // `Locked`, `LockedZooming` and `LockedGesture`. These tests pin the
    // transition edges and the resting states they converge to.

    #[test]
    fn fsm_idle_zoom_in_completes_to_idle() {
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        assert!(matches!(state, OutputZoomState::Idle(_)));

        state.zoom_in(2., Point::from((960., 540.)), &clock, test_anim_config());
        assert!(
            matches!(
                state,
                OutputZoomState::Zooming {
                    zooming: ZoomingState::Command(ZoomCommandState::In { target, .. }),
                    ..
                } if target == 2.
            ),
            "expected Zooming(Command(In)), got {}",
            state_name(&state)
        );
        assert_eq!(state.target_level(), 2.);
        assert_eq!(state.intent_level(), 2.);
        assert!(state.is_zooming());
        assert!(state.is_animating());
        assert!(!state.is_following());
        assert!(!state.is_gesturing());

        advance(&mut state, &mut clock, 5000);
        assert!(
            matches!(state, OutputZoomState::Idle(_)),
            "expected Idle, got {}",
            state_name(&state)
        );
        assert_eq!(state.level(), 2.);
        assert_eq!(state.intent_level(), 2.);
    }

    #[test]
    fn fsm_command_targets() {
        // Each command kind carries its resolved target: In/Out from
        // incremental actions, Value from an explicit level, ToIdentity for
        // the normalized zoom-out-to-1x.
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let clock = test_clock();
        let anchor = Point::from((960., 540.));

        state.zoom_in(2., anchor, &clock, test_anim_config());
        assert!(matches!(
            state,
            OutputZoomState::Zooming {
                zooming: ZoomingState::Command(ZoomCommandState::In { target, .. }),
                ..
            } if target == 2.
        ));

        state.zoom_out(1.5, anchor, &clock, test_anim_config());
        assert!(matches!(
            state,
            OutputZoomState::Zooming {
                zooming: ZoomingState::Command(ZoomCommandState::Out { target, .. }),
                ..
            } if target == 1.5
        ));

        state.set_target_level(3., anchor, &clock, test_anim_config());
        assert!(matches!(
            state,
            OutputZoomState::Zooming {
                zooming: ZoomingState::Command(ZoomCommandState::Value { target, .. }),
                ..
            } if target == 3.
        ));

        state.set_target_level(1., anchor, &clock, test_anim_config());
        assert!(matches!(
            state,
            OutputZoomState::Zooming {
                zooming: ZoomingState::Command(ZoomCommandState::ToIdentity { .. }),
                ..
            }
        ));
        assert_eq!(state.target_level(), 1.);
        assert_eq!(state.intent_level(), 1.);
    }

    #[test]
    fn fsm_command_direction_uses_intent_not_display() {
        // The command direction is resolved from the user intent (the
        // requested operation), not by comparing the target against the
        // displayed level. Here the displayed level is still below the
        // zoom-out target, yet the command is an Out.
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let anchor = Point::from((960., 540.));

        state.zoom_in(4., anchor, &clock, test_anim_config());
        advance(&mut state, &mut clock, 10);
        let displayed = state.level();
        assert!(displayed > 1. && displayed < 2.);

        state.zoom_out(2., anchor, &clock, test_anim_config());
        assert!(
            matches!(
                state,
                OutputZoomState::Zooming {
                    zooming: ZoomingState::Command(ZoomCommandState::Out { target, .. }),
                    ..
                } if target == 2.
            ),
            "expected Out command, got {}",
            state_name(&state)
        );
        // No jump: the displayed level continues from where it was.
        assert_abs_diff_eq!(state.level(), displayed, epsilon = EPS);
    }

    #[test]
    fn fsm_repeated_action_replaces_value_with_in() {
        // A repeated incremental action replaces the in-flight command: the
        // new target is derived from the intent level, not the displayed
        // one, and the command kind changes Value → In.
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let anchor = Point::from((960., 540.));

        state.set_target_level(2., anchor, &clock, test_anim_config());
        assert!(matches!(
            state,
            OutputZoomState::Zooming {
                zooming: ZoomingState::Command(ZoomCommandState::Value { .. }),
                ..
            }
        ));
        advance(&mut state, &mut clock, 50);
        let displayed = state.level();
        assert!(displayed > 1. && displayed < 2.);

        // The input layer resolves the next target from intent_level():
        // 2. * 1.5 = 3.
        assert_eq!(state.intent_level(), 2.);
        state.zoom_in(3., anchor, &clock, test_anim_config());
        assert!(matches!(
            state,
            OutputZoomState::Zooming {
                zooming: ZoomingState::Command(ZoomCommandState::In { target, .. }),
                ..
            } if target == 3.
        ));
        assert_abs_diff_eq!(state.level(), displayed, epsilon = EPS);

        advance(&mut state, &mut clock, 5000);
        assert_eq!(state.level(), 3.);
        assert!(matches!(state, OutputZoomState::Idle(_)));
    }

    #[test]
    fn fsm_to_identity_lands_exactly() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);
        let mut clock = test_clock();

        state.set_target_level(1., Point::from((960., 540.)), &clock, test_anim_config());
        assert!(matches!(
            state,
            OutputZoomState::Zooming {
                zooming: ZoomingState::Command(ZoomCommandState::ToIdentity { .. }),
                ..
            }
        ));

        advance(&mut state, &mut clock, 5000);
        assert!(matches!(state, OutputZoomState::Idle(_)));
        assert_eq!(state.level(), 1.);
        assert_eq!(state.viewport_transform().factor(), 1.);
    }

    #[test]
    fn fsm_follow_starts_and_converges() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);
        let mut clock = test_clock();
        let zoom = niri_config::Zoom {
            deadzone_size: 0.5,
            ..Default::default()
        };
        let cursor = Point::from((100., 540.));

        assert!(state.update_follow(cursor, zoom, &clock, test_anim_config()));
        assert!(
            matches!(state, OutputZoomState::Follow { .. }),
            "expected Follow, got {}",
            state_name(&state)
        );
        assert!(state.is_following());
        assert!(!state.is_zooming());

        // The follow converges to the clamped deadzone target and rests.
        for _ in 0..500 {
            advance(&mut state, &mut clock, 16);
            state.update_follow(cursor, zoom, &clock, test_anim_config());
            if matches!(state, OutputZoomState::Idle(_)) {
                break;
            }
        }
        assert!(
            matches!(state, OutputZoomState::Idle(_)),
            "expected Idle, got {}",
            state_name(&state)
        );
        assert_eq!(state.level(), 2.);
    }

    #[test]
    fn fsm_zoom_action_during_follow() {
        // A zoom action during a resting follow keeps the follow alive: the
        // state becomes ZoomingFollow immediately, the camera does not
        // jump, and the follow keeps converging during the level animation.
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);
        let mut clock = test_clock();
        let zoom = niri_config::Zoom {
            deadzone_size: 0.5,
            ..Default::default()
        };
        let cursor = Point::from((100., 540.));

        assert!(state.update_follow(cursor, zoom, &clock, test_anim_config()));
        advance(&mut state, &mut clock, 16);
        state.update_follow(cursor, zoom, &clock, test_anim_config());
        assert!(matches!(state, OutputZoomState::Follow { .. }));
        let focal = state.focal();
        let level = state.level();

        state.zoom_in(3., Point::from((960., 540.)), &clock, test_anim_config());
        assert!(
            matches!(
                state,
                OutputZoomState::ZoomingFollow {
                    zooming: ZoomCommandState::In { target, .. },
                    ..
                } if target == 3.
            ),
            "expected ZoomingFollow(In), got {}",
            state_name(&state)
        );
        assert!(state.is_following());
        assert_abs_diff_eq!(state.level(), level, epsilon = EPS);
        assert_point_eq(state.focal(), focal);

        // The follow is still live: the next evaluation steps it.
        advance(&mut state, &mut clock, 16);
        assert!(state.update_follow(cursor, zoom, &clock, test_anim_config()));
        assert!(matches!(state, OutputZoomState::ZoomingFollow { .. }));
    }

    #[test]
    fn fsm_resting_command_noop() {
        // A command whose target equals the resting level is an exact
        // no-op: no zero-distance Zooming state is created.
        let view_size = Size::from((1920., 1080.));
        let clock = test_clock();
        let anchor = Point::from((960., 540.));

        // Idle at 1x: zooming out to 1 and setting level 1 are no-ops.
        let mut state = OutputZoomState::new(view_size);
        state.zoom_out(1., anchor, &clock, test_anim_config());
        assert!(matches!(state, OutputZoomState::Idle(_)));
        state.set_target_level(1., anchor, &clock, test_anim_config());
        assert!(matches!(state, OutputZoomState::Idle(_)));
        assert!(!state.is_animating());

        // Idle at 2x: an explicit set to the resting level is a no-op.
        let mut state = state_at(2., Point::from((960., 540.)), view_size);
        state.set_target_level(2., anchor, &clock, test_anim_config());
        assert!(matches!(state, OutputZoomState::Idle(_)));
        assert!(!state.is_animating());
        assert_eq!(state.level(), 2.);

        // Locked resting at 2x: same no-op, stays Locked.
        let mut state = state_at(2., Point::from((960., 540.)), view_size);
        state.set_locked(true, anchor);
        state.set_target_level(2., anchor, &clock, test_anim_config());
        assert!(matches!(state, OutputZoomState::Locked(_)));
        assert!(!state.is_animating());

        // Resting follow: a no-op command does not disturb it.
        let zoom = niri_config::Zoom {
            deadzone_size: 0.5,
            ..Default::default()
        };
        let mut state = state_at(2., Point::from((960., 540.)), view_size);
        assert!(state.update_follow(Point::from((100., 540.)), zoom, &clock, test_anim_config()));
        assert!(matches!(state, OutputZoomState::Follow { .. }));
        state.set_target_level(2., anchor, &clock, test_anim_config());
        assert!(
            matches!(state, OutputZoomState::Follow { .. }),
            "expected Follow, got {}",
            state_name(&state)
        );
        assert!(state.is_following());
    }

    #[test]
    #[cfg(debug_assertions)]
    fn fsm_command_direction_rejects_wrong_side() {
        // The caller resolves the target from the intent level; a zoom-in
        // target below the intent (or a zoom-out target above it) is a
        // caller bug and is rejected in debug builds.
        let view_size = Size::from((1920., 1080.));
        let clock = test_clock();
        let anchor = Point::from((960., 540.));

        let mut state = state_at(2., Point::from((960., 540.)), view_size);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            state.zoom_in(1.5, anchor, &clock, test_anim_config())
        }))
        .is_err());
        assert!(matches!(state, OutputZoomState::Idle(_)));
        assert_eq!(state.level(), 2.);

        let mut state = state_at(2., Point::from((960., 540.)), view_size);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            state.zoom_out(3., anchor, &clock, test_anim_config())
        }))
        .is_err());
        assert!(matches!(state, OutputZoomState::Idle(_)));
        assert_eq!(state.level(), 2.);
    }

    #[test]
    fn fsm_to_identity_during_follow_drops_follow() {
        // A command to the identity transform cannot keep a follow: at 1x
        // the deadzone is meaningless, so the combined state degrades to a
        // plain ToIdentity command with the focal point fixed at its live
        // displayed value.
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);
        let mut clock = test_clock();
        let zoom = niri_config::Zoom {
            deadzone_size: 0.5,
            ..Default::default()
        };

        assert!(state.update_follow(Point::from((100., 540.)), zoom, &clock, test_anim_config()));
        assert!(matches!(state, OutputZoomState::Follow { .. }));
        let focal = state.focal();

        state.set_target_level(1., Point::from((960., 540.)), &clock, test_anim_config());
        assert!(
            matches!(
                state,
                OutputZoomState::Zooming {
                    zooming: ZoomingState::Command(ZoomCommandState::ToIdentity { .. }),
                    ..
                }
            ),
            "expected Zooming(ToIdentity), got {}",
            state_name(&state)
        );
        assert!(!state.is_following());
        assert_point_eq(state.focal(), focal);

        advance(&mut state, &mut clock, 5000);
        assert!(matches!(state, OutputZoomState::Idle(_)));
        assert_eq!(state.level(), 1.);
    }

    #[test]
    fn fsm_zooming_follow_retarget_keeps_follow() {
        // Retargeting the combined state replaces the command but keeps the
        // follow: same live geometry, no stale timing, no jump.
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let zoom = niri_config::Zoom {
            deadzone_size: 0.5,
            ..Default::default()
        };
        let config = test_anim_config();
        let cursor = Point::from((100., 540.));

        state.zoom_in(4., Point::from((960., 540.)), &clock, config);
        advance(&mut state, &mut clock, 50);
        assert!(state.update_follow(cursor, zoom, &clock, config));
        assert!(matches!(state, OutputZoomState::ZoomingFollow { .. }));
        let level = state.level();
        let focal = state.focal();

        // zoom-out to a level still above 1: the command kind and target
        // change, the follow is retained.
        state.zoom_out(2., cursor, &clock, config);
        assert!(
            matches!(
                state,
                OutputZoomState::ZoomingFollow {
                    zooming: ZoomCommandState::Out { target, .. },
                    ..
                } if target == 2.
            ),
            "expected ZoomingFollow(Out), got {}",
            state_name(&state)
        );
        assert!(state.is_following());
        assert_eq!(state.target_level(), 2.);
        assert_abs_diff_eq!(state.level(), level, epsilon = EPS);
        assert_point_eq(state.focal(), focal);

        // The follow still steps on the next evaluation.
        advance(&mut state, &mut clock, 16);
        state.update_follow(cursor, zoom, &clock, config);
        assert!(state.is_following());
    }

    #[test]
    fn fsm_zooming_follow_retarget_to_identity_drops_follow() {
        // Retargeting the combined state to 1x drops the follow: the
        // deadzone is meaningless at the identity transform.
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let zoom = niri_config::Zoom {
            deadzone_size: 0.5,
            ..Default::default()
        };
        let config = test_anim_config();
        let cursor = Point::from((100., 540.));

        state.zoom_in(4., Point::from((960., 540.)), &clock, config);
        advance(&mut state, &mut clock, 50);
        assert!(state.update_follow(cursor, zoom, &clock, config));
        assert!(matches!(state, OutputZoomState::ZoomingFollow { .. }));
        let focal = state.focal();

        state.set_target_level(1., Point::from((960., 540.)), &clock, config);
        assert!(
            matches!(
                state,
                OutputZoomState::Zooming {
                    zooming: ZoomingState::Command(ZoomCommandState::ToIdentity { .. }),
                    ..
                }
            ),
            "expected Zooming(ToIdentity), got {}",
            state_name(&state)
        );
        assert!(!state.is_following());
        assert_point_eq(state.focal(), focal);

        advance(&mut state, &mut clock, 5000);
        assert_eq!(state.level(), 1.);
        assert!(matches!(state, OutputZoomState::Idle(_)));
    }

    #[test]
    fn fsm_zooming_follow_combined_then_resting_follow() {
        // A cursor outside the deadzone during a level animation combines
        // the command with a live follow: ZoomingFollow. When the command
        // completes first, the state degrades to a resting Follow that
        // keeps converging — without consuming the combined phase as one
        // stale step.
        let view_size = Size::from((1920., 1080.));
        let output = Rectangle::from_size(view_size);
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let zoom = niri_config::Zoom {
            deadzone_size: 0.5,
            ..Default::default()
        };
        let config = test_anim_config();
        let cursor = Point::from((100., 540.));

        state.zoom_in(2., Point::from((960., 540.)), &clock, config);
        advance(&mut state, &mut clock, 50);

        assert!(state.update_follow(cursor, zoom, &clock, config));
        assert!(
            matches!(state, OutputZoomState::ZoomingFollow { .. }),
            "expected ZoomingFollow, got {}",
            state_name(&state)
        );
        assert!(state.is_zooming());
        assert!(state.is_following());

        // Drive the combined state for a few frames: enough that a stale
        // resting-step timestamp would produce a clearly visible snap, but
        // few enough that the follow has not converged yet.
        for _ in 0..10 {
            advance(&mut state, &mut clock, 16);
            state.update_follow(cursor, zoom, &clock, config);
        }
        assert!(matches!(state, OutputZoomState::ZoomingFollow { .. }));

        // Complete the command instantly; the follow is still converging.
        clock.set_complete_instantly(true);
        state.advance_animations();
        clock.set_complete_instantly(false);
        assert_eq!(state.level(), 2.);
        assert!(
            matches!(state, OutputZoomState::Follow { .. }),
            "expected resting Follow, got {}",
            state_name(&state)
        );

        // The first resting step measures only its own frame delta: a stale
        // timestamp would snap the camera to the deadzone border at once.
        let before = state.viewport_transform().apply(cursor);
        advance(&mut state, &mut clock, 16);
        state.update_follow(cursor, zoom, &clock, config);
        let after = state.viewport_transform().apply(cursor);
        let d = after - before;
        let step = (d.x * d.x + d.y * d.y).sqrt();
        assert!(step < 100., "stale follow step: {step}px");

        // The resting follow converges normally.
        for _ in 0..500 {
            advance(&mut state, &mut clock, 16);
            state.update_follow(cursor, zoom, &clock, config);
            if matches!(state, OutputZoomState::Idle(_)) {
                break;
            }
        }
        assert!(matches!(state, OutputZoomState::Idle(_)));
        assert_viewport_within(state.viewport(), output);
    }

    #[test]
    fn fsm_zooming_follow_follow_converges_first() {
        // When the follow reaches its target while the command is still
        // animating, the combined state drops the follow and continues as a
        // plain command.
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let zoom = niri_config::Zoom {
            deadzone_size: 0.5,
            ..Default::default()
        };
        // A long easing keeps the command animating after the follow lands.
        let config = easing_config(5000);
        let cursor = Point::from((100., 540.));

        state.zoom_in(2., Point::from((960., 540.)), &clock, config);
        advance(&mut state, &mut clock, 50);
        assert!(state.update_follow(cursor, zoom, &clock, config));
        assert!(matches!(state, OutputZoomState::ZoomingFollow { .. }));

        // An instant follow evaluation lands the camera on the deadzone
        // edge while the level animation continues.
        state.update_follow(cursor, zoom, &clock, niri_config::Animation::new_off());
        assert!(
            matches!(
                state,
                OutputZoomState::Zooming {
                    zooming: ZoomingState::Command(_),
                    ..
                }
            ),
            "expected Zooming(Command), got {}",
            state_name(&state)
        );
        assert!(state.is_zooming());
        assert!(!state.is_following());
        assert!(state.is_animating());

        advance(&mut state, &mut clock, 6000);
        assert_eq!(state.level(), 2.);
        assert!(matches!(state, OutputZoomState::Idle(_)));
    }

    #[test]
    fn fsm_zooming_follow_simultaneous_completion() {
        // Command and follow completing together land on the resting zoomed
        // state: no follow is left running at the target.
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let zoom = niri_config::Zoom {
            deadzone_size: 0.5,
            ..Default::default()
        };
        let cursor = Point::from((100., 540.));

        state.zoom_in(2., Point::from((960., 540.)), &clock, test_anim_config());
        advance(&mut state, &mut clock, 50);
        assert!(state.update_follow(cursor, zoom, &clock, test_anim_config()));
        assert!(matches!(state, OutputZoomState::ZoomingFollow { .. }));

        // Land the follow on its target and complete the command in the
        // same frame.
        state.update_follow(cursor, zoom, &clock, niri_config::Animation::new_off());
        clock.set_complete_instantly(true);
        state.advance_animations();
        clock.set_complete_instantly(false);

        assert!(
            matches!(state, OutputZoomState::Idle(_)),
            "expected Idle, got {}",
            state_name(&state)
        );
        assert_eq!(state.level(), 2.);
        assert!(!state.is_following());
        assert!(!state.is_animating());
    }

    #[test]
    fn fsm_restore_pointer_takeover() {
        // Pointer interaction during a restore takes over the camera: the
        // restore becomes a regular command anchored on the cursor at its
        // current displayed position. The level keeps animating towards the
        // restore target without a focal jump.
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(3., Point::from((400., 300.)), view_size);
        let mut clock = test_clock();
        let zoom = niri_config::Zoom {
            deadzone_size: 0.5,
            ..Default::default()
        };
        let config = test_anim_config();

        let mut snapshot = snapshot_of(&state);
        snapshot.target_level = 1.5;
        snapshot.focal = Point::from((800., 500.));
        state.restore_animated(snapshot, &clock, config);
        assert!(
            matches!(
                state,
                OutputZoomState::Zooming {
                    zooming: ZoomingState::Restore(_),
                    ..
                }
            ),
            "expected Zooming(Restore), got {}",
            state_name(&state)
        );
        advance(&mut state, &mut clock, 50);
        let level = state.level();
        let focal = state.focal();

        // The per-frame follow evaluation never disturbs a restore.
        assert!(!state.update_follow(Point::from((0., 0.)), zoom, &clock, config));
        assert!(matches!(
            state,
            OutputZoomState::Zooming {
                zooming: ZoomingState::Restore(_),
                ..
            }
        ));

        // Pointer interaction takes over: restore → command, continuous.
        let cursor = Point::from((1500., 900.));
        assert!(state.update_focal_for_cursor(cursor, zoom, &clock, config));
        assert!(
            matches!(
                state,
                OutputZoomState::Zooming {
                    zooming: ZoomingState::Command(_),
                    ..
                } | OutputZoomState::ZoomingFollow { .. }
            ),
            "expected command takeover, got {}",
            state_name(&state)
        );
        assert_eq!(state.target_level(), 1.5);
        assert_abs_diff_eq!(state.level(), level, epsilon = EPS);
        assert_point_eq(state.focal(), focal);

        advance(&mut state, &mut clock, 5000);
        assert_eq!(state.level(), 1.5);
    }

    #[test]
    fn fsm_restore_replaces_follow() {
        // A restore owns the viewport exclusively: starting one during a
        // resting follow materializes the camera and takes over.
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);
        let clock = test_clock();
        let zoom = niri_config::Zoom {
            deadzone_size: 0.5,
            ..Default::default()
        };

        assert!(state.update_follow(Point::from((100., 540.)), zoom, &clock, test_anim_config()));
        assert!(matches!(state, OutputZoomState::Follow { .. }));
        let focal = state.focal();

        let mut snapshot = snapshot_of(&state);
        snapshot.target_level = 3.;
        snapshot.focal = Point::from((400., 300.));
        state.restore_animated(snapshot, &clock, test_anim_config());

        assert!(matches!(
            state,
            OutputZoomState::Zooming {
                zooming: ZoomingState::Restore(_),
                ..
            }
        ));
        // The camera did not jump when the follow was materialized.
        assert_point_eq(state.focal(), focal);
    }

    #[test]
    fn fsm_gesture_lifecycle() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        state.begin_gesture(Point::from((700., 400.)));
        assert!(matches!(state, OutputZoomState::Gesture { .. }));
        assert!(state.is_gesturing());
        assert!(!state.is_animating());
        assert!(!state.is_zooming());

        assert!(state.update_gesture(1.5, 10.));
        assert_eq!(state.level(), 3.);
        assert_eq!(state.intent_level(), 3.);

        state.end_gesture();
        assert!(matches!(state, OutputZoomState::Idle(_)));
        assert_eq!(state.level(), 3.);
        assert_eq!(state.target_level(), 3.);
    }

    #[test]
    fn fsm_gesture_begin_during_follow() {
        // A gesture takes over from a resting follow: the camera is
        // materialized and the gesture owns the viewport.
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);
        let clock = test_clock();
        let zoom = niri_config::Zoom {
            deadzone_size: 0.5,
            ..Default::default()
        };

        assert!(state.update_follow(Point::from((100., 540.)), zoom, &clock, test_anim_config()));
        assert!(matches!(state, OutputZoomState::Follow { .. }));
        let focal = state.focal();

        state.begin_gesture(Point::from((700., 400.)));
        assert!(matches!(state, OutputZoomState::Gesture { .. }));
        assert_point_eq(state.focal(), focal);
    }

    #[test]
    fn fsm_gesture_begin_during_zooming_follow() {
        // A gesture takes over the combined state: the command and the
        // follow are abandoned at the displayed viewport, which becomes the
        // gesture base.
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let zoom = niri_config::Zoom {
            deadzone_size: 0.5,
            ..Default::default()
        };
        let config = test_anim_config();

        state.zoom_in(2., Point::from((960., 540.)), &clock, config);
        advance(&mut state, &mut clock, 50);
        assert!(state.update_follow(Point::from((100., 540.)), zoom, &clock, config));
        assert!(matches!(state, OutputZoomState::ZoomingFollow { .. }));
        let level = state.level();
        let focal = state.focal();

        state.begin_gesture(Point::from((700., 400.)));
        assert!(matches!(state, OutputZoomState::Gesture { .. }));
        assert!(state.is_gesturing());
        assert_abs_diff_eq!(state.level(), level, epsilon = EPS);
        assert_point_eq(state.focal(), focal);
    }

    #[test]
    fn fsm_gesture_begin_during_locked_zooming() {
        // A gesture while a locked command is animating takes over into the
        // locked gesture state; the lock is kept.
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);
        let mut clock = test_clock();

        state.set_locked(true, Point::from((960., 540.)));
        state.zoom_in(4., Point::from((960., 540.)), &clock, test_anim_config());
        assert!(matches!(state, OutputZoomState::LockedZooming { .. }));
        advance(&mut state, &mut clock, 50);
        let level = state.level();

        state.begin_gesture(Point::from((700., 400.)));
        assert!(matches!(state, OutputZoomState::LockedGesture { .. }));
        assert!(state.is_gesturing());
        assert!(state.is_locked());
        assert_abs_diff_eq!(state.level(), level, epsilon = EPS);
    }

    #[test]
    fn fsm_locked_gesture_lifecycle() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);
        state.set_locked(true, Point::from((960., 540.)));
        assert!(matches!(state, OutputZoomState::Locked(_)));

        state.begin_gesture(Point::from((700., 400.)));
        assert!(matches!(state, OutputZoomState::LockedGesture { .. }));
        assert!(state.is_gesturing());
        assert!(state.is_locked());

        assert!(state.update_gesture(1.5, 10.));
        assert_eq!(state.level(), 3.);

        state.end_gesture();
        assert!(matches!(state, OutputZoomState::Locked(_)));
        assert!(state.is_locked());
        assert_eq!(state.level(), 3.);
    }

    #[test]
    fn fsm_lock_unlock_resting() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);
        let pointer = Point::from((1200., 700.));

        state.set_locked(true, pointer);
        assert!(matches!(state, OutputZoomState::Locked(_)));
        assert!(state.is_locked());
        // Locking a resting state does not move the camera.
        assert_point_eq(state.focal(), Point::from((960., 540.)));

        state.set_locked(false, pointer);
        assert!(matches!(state, OutputZoomState::Idle(_)));
        assert!(!state.is_locked());
        assert_point_eq(state.focal(), Point::from((960., 540.)));
    }

    #[test]
    fn fsm_lock_during_follow() {
        // Locking a resting follow materializes the camera where it is and
        // drops the follow: the combined locked+follow state does not
        // exist.
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);
        let mut clock = test_clock();
        let zoom = niri_config::Zoom {
            deadzone_size: 0.5,
            ..Default::default()
        };
        let cursor = Point::from((100., 540.));

        assert!(state.update_follow(cursor, zoom, &clock, test_anim_config()));
        advance(&mut state, &mut clock, 16);
        state.update_follow(cursor, zoom, &clock, test_anim_config());
        assert!(matches!(state, OutputZoomState::Follow { .. }));
        let focal = state.focal();

        state.set_locked(true, cursor);
        assert!(matches!(state, OutputZoomState::Locked(_)));
        assert!(!state.is_following());
        // The camera froze where the follow had carried it.
        assert_point_eq(state.focal(), focal);
    }

    #[test]
    fn fsm_lock_during_zooming_follow() {
        // Locking the combined state drops the follow and keeps the
        // command: LockedZooming. The displayed viewport does not jump.
        let view_size = Size::from((1920., 1080.));
        let mut state = OutputZoomState::new(view_size);
        let mut clock = test_clock();
        let zoom = niri_config::Zoom {
            deadzone_size: 0.5,
            ..Default::default()
        };
        let config = test_anim_config();
        let cursor = Point::from((100., 540.));

        state.zoom_in(2., Point::from((960., 540.)), &clock, config);
        advance(&mut state, &mut clock, 50);
        assert!(state.update_follow(cursor, zoom, &clock, config));
        assert!(matches!(state, OutputZoomState::ZoomingFollow { .. }));
        let viewport_before = state.viewport();

        state.set_locked(true, cursor);
        assert!(
            matches!(state, OutputZoomState::LockedZooming { .. }),
            "expected LockedZooming, got {}",
            state_name(&state)
        );
        assert!(!state.is_following());
        assert_eq!(state.target_level(), 2.);
        assert_point_eq(state.viewport().loc, viewport_before.loc);
        assert_abs_diff_eq!(
            state.viewport().size.w,
            viewport_before.size.w,
            epsilon = EPS
        );

        advance(&mut state, &mut clock, 5000);
        assert!(matches!(state, OutputZoomState::Locked(_)));
        assert_eq!(state.level(), 2.);
    }

    #[test]
    fn fsm_end_session_from_every_state() {
        // The Overview replaces the desktop presentation: from every FSM
        // state the session ends on the resting 1x identity, unlocked.
        let view_size = Size::from((1920., 1080.));
        let zoom = niri_config::Zoom {
            deadzone_size: 0.5,
            ..Default::default()
        };
        let mut clock = test_clock();

        let resting = || state_at(2., Point::from((960., 540.)), view_size);

        let mut follow = resting();
        follow.update_follow(Point::from((100., 540.)), zoom, &clock, test_anim_config());

        let mut command = resting();
        command.zoom_in(4., Point::from((960., 540.)), &clock, test_anim_config());

        let mut restore = resting();
        let mut snapshot = restore.snapshot();
        snapshot.target_level = 1.5;
        snapshot.focal = Point::from((400., 300.));
        restore.restore_animated(snapshot, &clock, test_anim_config());

        let mut combined = resting();
        combined.zoom_in(4., Point::from((960., 540.)), &clock, test_anim_config());
        advance(&mut combined, &mut clock, 50);
        combined.update_follow(Point::from((100., 540.)), zoom, &clock, test_anim_config());

        let mut gesture = resting();
        gesture.begin_gesture(Point::from((700., 400.)));

        let mut locked = resting();
        locked.set_locked(true, Point::from((960., 540.)));

        let mut locked_command = resting();
        locked_command.set_locked(true, Point::from((960., 540.)));
        locked_command.zoom_in(4., Point::from((960., 540.)), &clock, test_anim_config());

        let mut locked_gesture = resting();
        locked_gesture.set_locked(true, Point::from((960., 540.)));
        locked_gesture.begin_gesture(Point::from((700., 400.)));

        let mut states = [
            ("Idle", resting()),
            ("Follow", follow),
            ("Zooming(Command)", command),
            ("Zooming(Restore)", restore),
            ("ZoomingFollow", combined),
            ("Gesture", gesture),
            ("Locked", locked),
            ("LockedZooming", locked_command),
            ("LockedGesture", locked_gesture),
        ];

        for (expected, state) in states.iter_mut() {
            let expected = *expected;
            assert_eq!(
                state_name(state),
                expected,
                "setup did not reach {expected}"
            );
            state.end_session();
            assert!(
                matches!(state, OutputZoomState::Idle(_)),
                "end_session from {expected} left {}",
                state_name(state)
            );
            assert_eq!(state.level(), 1., "level after end_session from {expected}");
            assert_eq!(state.target_level(), 1.);
            assert!(!state.is_locked());
            assert!(!state.is_animating());
            assert!(!state.is_gesturing());
            assert!(!state.is_following());
            assert_eq!(state.viewport_transform().factor(), 1.);
        }
    }

    proptest! {
        #[test]
        fn output_zoom_state_viewport_stays_within_output(
            level in 1.01f64..8.,
            fx in 0f64..1920.,
            fy in 0f64..1080.,
            cx in 0f64..1920.,
            cy in 0f64..1080.,
            deadzone in 0f64..=1.,
        ) {
            let view_size = Size::from((1920., 1080.));
            let output = Rectangle::from_size(view_size);
            let mut state = state_at(level, Point::from((fx, fy)), view_size);

            track(&mut state, Point::from((cx, cy)), deadzone);

            assert_finite_point(state.focal());
            let viewport = state.viewport();
            assert_finite_rect(viewport);
            assert_viewport_within(viewport, output);
        }
    }
}
