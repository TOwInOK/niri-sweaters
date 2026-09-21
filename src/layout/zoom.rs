//! Per-output desktop zoom state.
//!
//! Holds the canonical zoom state for a single output and implements the
//! viewport/deadzone math on top of [`ViewportTransform`]. All coordinates are
//! output-local logical. Rendering and input integration live elsewhere.

use std::time::Duration;

use smithay::utils::{Logical, Point, Rectangle, Size};
use tracing::trace;

use crate::animation::{Animation, Clock};
use crate::utils::view::ViewportTransform;

/// Desktop zoom state of a single output.
///
/// `level` is the committed zoom level, `target_level` is the user-requested
/// level that future continuous actions and animations operate on. `focal` is
/// the fixed point of the viewport transform; it is kept within the output so
/// that the logical viewport never leaves the output bounds. `locked`
/// disables focal tracking.
///
/// While a [`ZoomLevelTransition`] is active, [`level()`](Self::level) and
/// [`focal()`](Self::focal) report the animated presentation values; the
/// committed fields are only updated when the transition completes.
#[derive(Debug, Clone)]
pub struct OutputZoomState {
    level: f64,
    target_level: f64,
    focal: Point<f64, Logical>,
    locked: bool,
    /// Output-local logical size of the output.
    ///
    /// Needed to clamp derived focal points in the accessors.
    view_size: Size<f64, Logical>,
    /// In-progress zoom level transition, if any.
    transition: Option<ZoomLevelTransition>,
}

/// An in-progress zoom level transition.
///
/// The level animates in `log2` space so that equal multiplicative steps
/// (1x→2x and 2x→4x) cover equal animation distance.
#[derive(Debug, Clone)]
enum ZoomLevelTransition {
    /// Driven by an [`Animation`] over `log2(level)`.
    Animation {
        animation: Animation,
        /// How the focal point behaves while the level animates.
        focal: ZoomTransitionFocal,
        /// Deadzone drift of the anchor's displayed position, if any.
        ///
        /// While the displayed cursor is outside the deadzone during a level
        /// transition, the anchor's display position moves towards the
        /// deadzone boundary with the shared distance-driven follow physics,
        /// so the camera starts following immediately instead of waiting for
        /// the level animation to finish. The value is the timestamp of the
        /// last drift step, used to compute the frame delta.
        display_follow: Option<Duration>,
    },
    /// A viewport restore driven by an [`Animation`] over progress `0 → 1`.
    ///
    /// Used to return from a `hold-zoom` override: unlike [`Self::Animation`],
    /// which keeps an action anchor in place, a restore moves the whole
    /// viewport back to a saved state. The level still animates in `log2`
    /// space between `from_level` and `to_level`, while the focal point
    /// interpolates linearly between `from_focal` and `to_focal`. When
    /// restoring to 1x the focal point stays at `from_focal` instead: the
    /// destination focal point is degenerate at the identity transform, so
    /// moving it early would only pan the viewport.
    Restore {
        animation: Animation,
        from_level: f64,
        to_level: f64,
        from_focal: Point<f64, Logical>,
        to_focal: Point<f64, Logical>,
        /// Clock and config captured at restore start, needed to convert the
        /// restore back into a regular level animation when user input takes
        /// over the camera mid-flight.
        clock: Clock,
        config: niri_config::Animation,
    },
    Gesturing {
        /// The displayed level when the gesture began.
        start_level: f64,
        /// The level set by the latest gesture update.
        current_level: f64,
        /// How the focal point behaves while the level changes.
        focal: ZoomTransitionFocal,
    },
    /// A deadzone-driven camera follow at a resting zoom level.
    ///
    /// The level is already at its target; only the focal point moves,
    /// driven by the shared distance-driven follow physics towards
    /// `to_focal`. A follow is mutually exclusive with the other
    /// transitions: it starts only once the level animation, restore or
    /// gesture that owned the viewport has finished.
    Follow {
        /// The clamped focal point the follow converges to.
        to_focal: Point<f64, Logical>,
        /// Timestamp of the last follow step.
        ///
        /// Owned by the follow itself: it is created with the follow and
        /// dies with it, so a follow can never consume time that elapsed
        /// before it started — while tracking was suspended, the compositor
        /// was idle, or a different transition owned the viewport.
        last_step: Duration,
    },
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

/// Focal point behavior during a zoom level transition.
#[derive(Debug, Clone, Copy)]
enum ZoomTransitionFocal {
    /// Keeps the `content` point at the `display` position while the level
    /// animates, subject to the output bounds clamp.
    ///
    /// Both points are output-local logical coordinates snapshotted at
    /// (re)target time: `display` is where `content` was shown when the
    /// transition started.
    Anchored {
        content: Point<f64, Logical>,
        display: Point<f64, Logical>,
    },

    /// The focal point stays fixed while the level animates.
    ///
    /// Used while the zoom is locked: the camera does not move, so the
    /// displayed content shifts relative to the screen instead.
    Fixed { focal: Point<f64, Logical> },
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

        Self {
            level: 1.,
            target_level: 1.,
            focal: view_size.to_point().downscale(2.),
            locked: false,
            view_size,
            transition: None,
        }
    }

    /// The currently displayed zoom level.
    ///
    /// While a transition is in progress this samples the animation; once the
    /// animation first reaches its target this returns `target_level` exactly.
    pub fn level(&self) -> f64 {
        let Some(transition) = &self.transition else {
            return self.level;
        };

        match transition {
            ZoomLevelTransition::Gesturing { current_level, .. } => *current_level,
            ZoomLevelTransition::Animation { animation, .. } => {
                if animation.is_clamped_done() {
                    // Commit the exact target rather than exp2(log2(target))
                    // so that level == 1.0 stays exact for the identity fast
                    // path.
                    return self.target_level;
                }
                // Clamp to the animation endpoints so that an underdamped
                // spring can never display a level past the target or below
                // the start.
                let z = animation.value().clamp(
                    animation.from().min(animation.to()),
                    animation.from().max(animation.to()),
                );
                z.exp2()
            }
            ZoomLevelTransition::Restore {
                animation,
                from_level,
                to_level,
                ..
            } => {
                if animation.is_clamped_done() {
                    return self.target_level;
                }
                // The progress animation is clamped to 0..=1, so the level
                // stays between the endpoints even for an underdamped spring.
                let p = animation.clamped_value().clamp(0., 1.);
                let z = from_level.log2() + (to_level.log2() - from_level.log2()) * p;
                z.exp2()
            }
            ZoomLevelTransition::Follow { .. } => {
                // The level is already resting at its target; a follow only
                // moves the focal point.
                self.level
            }
        }
    }

    /// The user-requested zoom level.
    ///
    /// May differ from [`level()`](Self::level) while a continuous action or
    /// animation is in progress.
    pub fn target_level(&self) -> f64 {
        self.target_level
    }

    /// The fixed point of the current viewport transform, in output-local
    /// logical coordinates.
    ///
    /// While a transition is in progress this is derived from the transition
    /// state at the current animated level.
    pub fn focal(&self) -> Point<f64, Logical> {
        let Some(transition) = &self.transition else {
            return self.focal;
        };

        match transition {
            ZoomLevelTransition::Animation { focal, .. }
            | ZoomLevelTransition::Gesturing { focal, .. } => match focal {
                // The anchor's display position already carries any deadzone
                // drift: it is mutated in place by the distance-driven step.
                ZoomTransitionFocal::Anchored { content, display } => {
                    self.anchored_focal(*content, *display, self.level())
                }
                ZoomTransitionFocal::Fixed { focal } => Self::clamp_focal(*focal, self.view_size),
            },
            ZoomLevelTransition::Restore {
                animation,
                from_focal,
                to_level,
                to_focal,
                ..
            } => {
                // Restoring to 1x: the destination focal point is visually
                // degenerate (the transform is the identity), so the focal
                // point stays at `from_focal` until the transition commits.
                // Interpolating it early would pan the viewport towards a
                // point that only matters once the level reaches 1.
                if *to_level == 1. && !animation.is_clamped_done() {
                    return Self::clamp_focal(*from_focal, self.view_size);
                }

                // The focal point interpolates with the clamped progress so
                // that it never overshoots the restore destination.
                let p = animation.clamped_value().clamp(0., 1.);
                let to_focal = Self::clamp_focal(*to_focal, self.view_size);
                let focal = Point::from((
                    from_focal.x + (to_focal.x - from_focal.x) * p,
                    from_focal.y + (to_focal.y - from_focal.y) * p,
                ));
                Self::clamp_focal(focal, self.view_size)
            }
            ZoomLevelTransition::Follow { .. } => {
                // The distance-driven follow mutates the committed focal
                // point in place every frame.
                Self::clamp_focal(self.focal, self.view_size)
            }
        }
    }

    /// Whether focal tracking is locked.
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// Whether a zoom level transition is in progress.
    ///
    /// A [`ZoomLevelTransition::Gesturing`] is not an animation: gesture
    /// events drive the displayed level directly and queue their own redraws.
    pub fn is_animating(&self) -> bool {
        match &self.transition {
            Some(ZoomLevelTransition::Animation {
                animation,
                display_follow,
                ..
            }) => !animation.is_clamped_done() || display_follow.is_some(),
            Some(ZoomLevelTransition::Restore { animation, .. }) => !animation.is_clamped_done(),
            // A distance-driven follow animates until it reaches its target.
            Some(ZoomLevelTransition::Follow { to_focal, .. }) => self.focal() != *to_focal,
            Some(ZoomLevelTransition::Gesturing { .. }) | None => false,
        }
    }

    /// Whether a direct-manipulation gesture currently owns the zoom level.
    ///
    /// While gesturing, [`level()`](Self::level) and
    /// [`target_level()`](Self::target_level) both report the level set by
    /// the latest gesture update.
    pub fn is_gesturing(&self) -> bool {
        matches!(self.transition, Some(ZoomLevelTransition::Gesturing { .. }))
    }

    /// Sets whether focal tracking is locked.
    ///
    /// Locking during a transition freezes the focal point at its current
    /// displayed value: the level animation continues, but the camera stops
    /// tracking the anchor. Unlocking keeps the current focal point; regular
    /// deadzone tracking can move it again on the next pointer event.
    pub fn set_locked(&mut self, locked: bool) {
        // A follow owns the focal point: materialize its current displayed
        // value first so the camera freezes where it actually is.
        self.commit_follow_focal();

        self.locked = locked;

        if locked {
            let current = self.focal();
            if matches!(self.transition, Some(ZoomLevelTransition::Restore { .. })) {
                // Locking during a restore discards the focal destination:
                // the camera freezes at its current position while the level
                // keeps animating towards the restore target.
                self.convert_restore_to_level_animation(ZoomTransitionFocal::Fixed {
                    focal: current,
                });
            } else {
                match &mut self.transition {
                    Some(ZoomLevelTransition::Animation {
                        focal,
                        display_follow,
                        ..
                    }) => {
                        *focal = ZoomTransitionFocal::Fixed { focal: current };
                        // The lock freezes the camera: drop any deadzone
                        // drift so it cannot keep moving under the lock.
                        *display_follow = None;
                    }
                    Some(ZoomLevelTransition::Gesturing { focal, .. }) => {
                        *focal = ZoomTransitionFocal::Fixed { focal: current };
                    }
                    _ => (),
                }
            }
        }
    }

    /// Toggles whether focal tracking is locked.
    pub fn toggle_locked(&mut self) {
        self.set_locked(!self.locked);
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

        if config.off {
            self.set_level_immediate(level, anchor);
            return;
        }

        // A follow owns the focal point: materialize its current displayed
        // value so the new level animation starts from the real camera
        // position, not the stale committed focal point.
        self.commit_follow_focal();

        let from_level = self.level();
        let velocity = self.transition_log_velocity();

        self.target_level = level;

        let focal = if level == 1. {
            // Zooming out to the identity transform: the anchored focal solve
            // degenerates as the level approaches 1 (it divides by
            // `level - 1`), so the focal point would fly to a clamped corner.
            // Keep the current focal point fixed instead: the zoom-out
            // pivots around its displayed position and lands continuously.
            ZoomTransitionFocal::Fixed {
                focal: self.focal(),
            }
        } else if self.locked {
            // A locked camera does not track the pointer: zoom around the
            // viewport center so the viewed content stays centered.
            let center = self.view_size.to_point().downscale(2.);
            ZoomTransitionFocal::Anchored {
                content: self.viewport_transform().apply_inverse(center),
                display: center,
            }
        } else {
            ZoomTransitionFocal::Anchored {
                content: anchor,
                display: self.viewport_transform().apply(anchor),
            }
        };

        // The deadzone drift is not seeded here: the per-frame follow
        // evaluation starts it on the first frame the displayed cursor is
        // outside the deadzone.
        self.transition = Some(ZoomLevelTransition::Animation {
            animation: Animation::new(
                clock.clone(),
                from_level.log2(),
                level.log2(),
                velocity,
                config,
            ),
            focal,
            display_follow: None,
        });
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
            .apply_inverse_rect(Rectangle::from_size(self.view_size))
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

        let deadzone = Self::deadzone_rect(self.view_size, deadzone_size);
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
        let focal = Self::clamp_focal(focal, self.view_size);

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
    /// restore converts to a regular level transition anchored at the
    /// cursor's current displayed position, so the level keeps animating
    /// towards the restore target without a focal jump. During a level
    /// animation the anchor owns the camera, but tracking still applies as a
    /// drift of the anchor's display position towards the deadzone edge, so
    /// the camera starts following the cursor during the zoom animation
    /// rather than after it.
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

        if self.locked || self.level() == 1. {
            return false;
        }

        // During a level transition the anchor owns the camera: retarget the
        // deadzone drift (or stop it when the cursor re-enters the deadzone)
        // instead of evaluating a resting-level follow.
        if matches!(self.transition, Some(ZoomLevelTransition::Animation { .. })) {
            return self.update_follow(cursor, zoom, clock, config);
        }

        let Some(_) = self.focal_target_for_cursor(cursor, zoom.deadzone_size) else {
            // The displayed cursor is inside the deadzone or already at the
            // target: an active follow has nothing left to do.
            return self.commit_follow_focal();
        };

        // During a restore the camera is heading back to a saved focal point.
        // User-driven tracking takes over: the restore becomes a regular level
        // transition anchored on the cursor at its current displayed position,
        // so the level keeps animating towards the restore target while the
        // camera stays continuous. The follow evaluation brings the cursor to
        // the deadzone edge once the level animation completes.
        if matches!(self.transition, Some(ZoomLevelTransition::Restore { .. })) {
            let display = self.viewport_transform().apply(cursor);
            self.convert_restore_to_level_animation(ZoomTransitionFocal::Anchored {
                content: cursor,
                display,
            });
            return true;
        }

        self.update_follow(cursor, zoom, clock, config)
    }

    /// Starts or retargets a deadzone follow for the cursor.
    ///
    /// This is the per-frame evaluation entry point. During a level
    /// transition it retargets the anchor's deadzone drift instead of
    /// starting a resting-level follow; a restore or gesture is never
    /// disturbed. When the displayed cursor is inside the deadzone or
    /// already at the clamped target, an active follow commits its current
    /// displayed focal point and stops.
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

        if self.locked {
            // A locked camera does not track the pointer: no deadzone drift
            // during a level transition, no resting-level follow.
            return false;
        }

        match &self.transition {
            // A level transition owns the camera through its anchor: retarget
            // the deadzone drift instead of starting a resting-level follow.
            Some(ZoomLevelTransition::Animation { .. }) => {
                return self.update_display_follow(cursor, zoom, clock, config);
            }
            // A restore or gesture owns the viewport.
            Some(ZoomLevelTransition::Restore { .. })
            | Some(ZoomLevelTransition::Gesturing { .. }) => return false,
            Some(ZoomLevelTransition::Follow { .. }) | None => (),
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
            self.view_size,
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
            self.focal = target;
            self.transition = None;
            return true;
        };

        // Start a follow if none is active, then apply the distance-driven
        // step for the elapsed time since the previous step. The timestamp
        // is owned by the follow: a new follow starts at `dt = 0` and can
        // never consume time that elapsed before it was created.
        if !matches!(self.transition, Some(ZoomLevelTransition::Follow { .. })) {
            self.transition = Some(ZoomLevelTransition::Follow {
                to_focal: target,
                last_step: now,
            });
        }
        let Some(ZoomLevelTransition::Follow {
            to_focal,
            last_step,
        }) = &mut self.transition
        else {
            unreachable!();
        };

        // The follow target tracks the live cursor.
        *to_focal = target;

        let dt = now.saturating_sub(*last_step).as_secs_f64();
        *last_step = now;
        let distance = input.speed * dt;
        let remaining = target - self.focal;
        let rem_len = (remaining.x * remaining.x + remaining.y * remaining.y).sqrt();

        if distance >= rem_len {
            self.focal = target;
            self.transition = None;
        } else {
            let scale = distance / rem_len;
            self.focal = Point::from((
                self.focal.x + remaining.x * scale,
                self.focal.y + remaining.y * scale,
            ));
        }
        true
    }

    /// Retargets the deadzone drift of an in-progress level transition.
    ///
    /// While the displayed cursor is outside the deadzone, the anchor is
    /// re-pinned on the live cursor and its display position steps towards
    /// the deadzone boundary with the shared distance-driven physics, so the
    /// camera follows the cursor during the level animation rather than
    /// after it. When the cursor re-enters the deadzone the drift stops:
    /// the display position is already live, so nothing needs materializing.
    ///
    /// Does nothing unless the current transition is an anchored level
    /// animation. Returns `true` if the state changed.
    fn update_display_follow(
        &mut self,
        cursor: Point<f64, Logical>,
        zoom: niri_config::Zoom,
        clock: &Clock,
        config: niri_config::Animation,
    ) -> bool {
        let display = self.viewport_transform().apply(cursor);
        let input = Self::deadzone_follow_input(
            self.view_size,
            zoom.deadzone_size,
            display,
            zoom.follow_min_speed,
            zoom.follow_max_speed,
        );

        // The drift only applies while the transition zooms in: at a target
        // level of 1 the deadzone is meaningless.
        let drift_allowed = self.target_level > 1. && input.is_some();

        // Whether the drift target is reachable at the current level: when
        // the output clamp already pins the camera, the anchor display
        // position cannot move and no drift is started.
        let reachable = drift_allowed
            && self.anchored_focal(cursor, input.unwrap().target, self.level()) != self.focal();

        let now = clock.now_unadjusted();
        let Some(ZoomLevelTransition::Animation {
            focal:
                ZoomTransitionFocal::Anchored {
                    content,
                    display: anchor_display,
                },
            display_follow,
            ..
        }) = &mut self.transition
        else {
            return false;
        };

        if !drift_allowed || !reachable {
            // The display position is live: dropping the drift marker stops
            // the camera exactly where it is.
            return display_follow.take().is_some();
        }
        let input = input.unwrap();

        if config.off || clock.should_complete_instantly() {
            // Instant completion: the drift jumps straight to the deadzone
            // border.
            *content = cursor;
            *anchor_display = input.target;
            *display_follow = None;
            return true;
        }
        // Re-anchor on the live cursor: the camera pans so that the cursor's
        // displayed position drifts towards the deadzone boundary.
        *content = cursor;

        let Some(last) = *display_follow else {
            *display_follow = Some(now);
            *anchor_display = display;
            return true;
        };

        let dt = now.saturating_sub(last).as_secs_f64();
        *display_follow = Some(now);

        let distance = input.speed * dt;
        if distance >= input.overshoot_length {
            *anchor_display = input.target;
            *display_follow = None;
        } else {
            *anchor_display = Point::from((
                display.x - input.direction.x * distance,
                display.y - input.direction.y * distance,
            ));
        }
        true
    }

    /// Commits an active follow's current displayed focal point.
    ///
    /// The distance-driven follow mutates the committed focal point in
    /// place every frame, so committing only drops the transition: the
    /// camera stays exactly where it is. Does nothing unless a follow is
    /// active.
    ///
    /// Returns `true` if a follow was committed.
    pub fn commit_follow_focal(&mut self) -> bool {
        if !matches!(self.transition, Some(ZoomLevelTransition::Follow { .. })) {
            return false;
        }

        self.transition = None;
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
        let drift = match &mut self.transition {
            Some(ZoomLevelTransition::Animation { display_follow, .. }) => {
                display_follow.take().is_some()
            }
            _ => false,
        };

        self.commit_follow_focal() || drift
    }

    /// Sets the focal point that the current state should display.
    ///
    /// Without a transition this writes the committed focal point. During an
    /// anchored transition it re-pins the anchor's display position so that
    fn set_current_focal(&mut self, focal: Point<f64, Logical>) -> bool {
        let focal = Self::clamp_focal(focal, self.view_size);
        if self.focal() == focal {
            return false;
        }

        // A follow has no anchor to re-pin: materialize its current
        // displayed focal point first so the write lands on the committed
        // focal point.
        self.commit_follow_focal();

        // A restore has no anchor to re-pin; it converts to a regular level
        // transition first so that the focal point can be set directly.
        if matches!(self.transition, Some(ZoomLevelTransition::Restore { .. })) {
            self.convert_restore_to_level_animation(ZoomTransitionFocal::Fixed {
                focal: self.focal(),
            });
        }

        let level = self.level();
        match &mut self.transition {
            Some(ZoomLevelTransition::Animation {
                focal: ZoomTransitionFocal::Anchored { content, display },
                display_follow,
                ..
            }) => {
                // Solve display = level * content - focal * (level - 1) so
                // that the anchored focal resolves to `focal` right now. A
                // deadzone drift is dropped: the explicit focal wins.
                *display = Point::from((
                    level * content.x - focal.x * (level - 1.),
                    level * content.y - focal.y * (level - 1.),
                ));
                *display_follow = None;
            }
            Some(ZoomLevelTransition::Gesturing {
                focal: ZoomTransitionFocal::Anchored { content, display },
                ..
            }) => {
                *display = Point::from((
                    level * content.x - focal.x * (level - 1.),
                    level * content.y - focal.y * (level - 1.),
                ));
            }
            Some(ZoomLevelTransition::Animation {
                focal: ZoomTransitionFocal::Fixed { focal: fixed },
                display_follow,
                ..
            }) => {
                *fixed = focal;
                *display_follow = None;
            }
            Some(ZoomLevelTransition::Gesturing {
                focal: ZoomTransitionFocal::Fixed { focal: fixed },
                ..
            }) => {
                *fixed = focal;
            }
            Some(ZoomLevelTransition::Restore { .. })
            | Some(ZoomLevelTransition::Follow { .. }) => unreachable!(),
            None => {
                self.focal = focal;
            }
        }

        true
    }

    /// Immediately sets the zoom level, keeping `anchor` at its current
    /// displayed position where the output bounds allow it.
    ///
    /// `anchor` is a content position in output-local logical coordinates,
    /// typically the cursor; while locked it is ignored and the viewport
    /// center is kept fixed instead. Also sets `target_level` to `level` and
    /// cancels any transition in progress.
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
        let anchor = if self.locked {
            let center = self.view_size.to_point().downscale(2.);
            self.viewport_transform().apply_inverse(center)
        } else {
            anchor
        };

        let screen_anchor = self.viewport_transform().apply(anchor);

        self.transition = None;
        self.level = level;
        self.target_level = level;

        if level == 1. {
            self.focal = Self::clamp_focal(self.focal, self.view_size);
            return;
        }

        // The focal point that keeps the content anchor at its previous
        // displayed position: screen_anchor = focal + (anchor - focal) * level.
        let focal = Point::from((
            (level * anchor.x - screen_anchor.x) / (level - 1.),
            (level * anchor.y - screen_anchor.y) / (level - 1.),
        ));
        self.focal = Self::clamp_focal(focal, self.view_size);
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
    /// `target_level` is set to the displayed level: during a gesture the
    /// displayed state is the current user intent.
    pub fn begin_gesture(&mut self, anchor: Point<f64, Logical>) {
        // A follow owns the focal point: materialize its current displayed
        // value so the gesture takes over the real camera position.
        self.commit_follow_focal();

        let level = self.level();
        self.target_level = level;

        let focal = if self.locked {
            // A locked camera does not track the pointer: zoom around the
            // viewport center so the viewed content stays centered.
            let center = self.view_size.to_point().downscale(2.);
            ZoomTransitionFocal::Anchored {
                content: self.viewport_transform().apply_inverse(center),
                display: center,
            }
        } else {
            ZoomTransitionFocal::Anchored {
                content: anchor,
                display: self.viewport_transform().apply(anchor),
            }
        };

        self.transition = Some(ZoomLevelTransition::Gesturing {
            start_level: level,
            current_level: level,
            focal,
        });
    }

    /// Applies a gesture update's cumulative scale to the zoom level.
    ///
    /// `scale` is relative to the gesture begin: the new level is
    /// `start_level * scale`, clamped to `1..=max_zoom` and snapped to
    /// exactly 1 within [`ZOOM_SNAP_TO_ONE_EPSILON`]. `target_level` tracks
    /// the displayed level.
    ///
    /// Non-finite or non-positive scales are ignored without disturbing the
    /// gesture. Does nothing unless a gesture is in progress.
    ///
    /// Returns `true` if the displayed state changed.
    pub fn update_gesture(&mut self, scale: f64, max_zoom: f64) -> bool {
        let Some(ZoomLevelTransition::Gesturing {
            start_level,
            current_level,
            ..
        }) = &mut self.transition
        else {
            return false;
        };

        if !scale.is_finite() || scale <= 0. {
            trace!("ignoring invalid pinch scale {scale}");
            return false;
        }

        let mut level = (*start_level * scale).clamp(1., max_zoom);
        if level <= 1. + ZOOM_SNAP_TO_ONE_EPSILON {
            level = 1.;
        }

        if *current_level == level {
            return false;
        }

        *current_level = level;
        self.target_level = level;
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

        self.level = self.target_level;
        self.focal = self.focal();
        self.transition = None;
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

        self.view_size = view_size;

        if self.level() == 1. {
            self.focal = view_size.to_point().downscale(2.);
        } else {
            self.focal = Self::clamp_focal(self.focal, view_size);
        }
    }

    /// Commits a finished transition, if any.
    ///
    /// Called from the monitor's `advance_animations`. When the transition
    /// first reaches its target, the exact `target_level` and the final
    /// clamped focal point are committed and the transition is dropped.
    pub fn advance_animations(&mut self) {
        let done = match &self.transition {
            Some(ZoomLevelTransition::Animation { animation, .. }) => animation.is_clamped_done(),
            Some(ZoomLevelTransition::Restore { animation, .. }) => animation.is_clamped_done(),
            // A distance-driven follow completes when the focal point
            // reaches its target; the step caps exactly at it.
            Some(ZoomLevelTransition::Follow { to_focal, .. }) => self.focal() == *to_focal,
            // A gesture is driven by input events, not the clock; it never
            // completes here.
            Some(ZoomLevelTransition::Gesturing { .. }) | None => return,
        };
        if !done {
            return;
        }

        self.level = self.target_level;
        self.focal = self.focal();
        self.transition = None;
    }

    /// Captures the restorable part of the zoom state.
    ///
    /// The snapshot contains the currently displayed `level` and `focal`, and
    /// the user-requested `target_level`. `locked` is deliberately excluded:
    /// it is a user preference, not part of a temporary viewport override.
    pub fn snapshot(&self) -> ZoomSnapshot {
        ZoomSnapshot {
            level: self.level(),
            target_level: self.target_level,
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

        self.view_size = view_size;
        self.transition = None;
        self.level = snapshot.target_level;
        self.target_level = snapshot.target_level;
        self.focal = Self::clamp_focal(snapshot.focal, view_size);
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
            let view_size = self.view_size;
            self.restore_immediate(snapshot, view_size);
            return;
        }

        let to_level = snapshot.target_level;
        let to_focal = Self::clamp_focal(snapshot.focal, self.view_size);

        if self.locked {
            // The lock is stronger than focal restoration: keep the camera
            // fixed and animate only the level towards the saved target.
            // `set_target_level` ignores the anchor while locked and zooms
            // around the viewport center.
            let anchor = self.view_size.to_point().downscale(2.);
            self.set_target_level(to_level, anchor, clock, config);
            return;
        }

        let from_level = self.level();
        let from_focal = self.focal();

        if from_level == to_level && from_focal == to_focal {
            // Already at the destination: commit it, cancelling any in-flight
            // transition.
            self.transition = None;
            self.level = to_level;
            self.target_level = to_level;
            self.focal = to_focal;
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

        self.target_level = to_level;
        self.transition = Some(ZoomLevelTransition::Restore {
            animation: Animation::new(clock.clone(), 0., 1., v_progress, config),
            from_level,
            to_level,
            from_focal,
            to_focal,
            clock: clock.clone(),
            config,
        });
    }

    /// The current log-space level velocity of the transition in progress.
    ///
    /// For a regular level animation this is the animation velocity directly.
    /// For a restore, the progress velocity is scaled by the level delta.
    /// Returns 0 when the velocity is unavailable (easing curves) or absent.
    fn transition_log_velocity(&self) -> f64 {
        match &self.transition {
            Some(ZoomLevelTransition::Animation { animation, .. }) => {
                animation.velocity().unwrap_or(0.)
            }
            Some(ZoomLevelTransition::Restore {
                animation,
                from_level,
                to_level,
                ..
            }) => {
                let dz = to_level.log2() - from_level.log2();
                animation.velocity().unwrap_or(0.) * dz
            }
            Some(ZoomLevelTransition::Gesturing { .. })
            | Some(ZoomLevelTransition::Follow { .. })
            | None => 0.,
        }
    }

    /// Converts an in-progress restore into a regular level animation.
    ///
    /// Used when user input takes over the camera mid-restore: the saved
    /// focal destination is abandoned, the level keeps animating towards the
    /// restore target with its current velocity, and the focal point follows
    /// `focal` behavior from the current displayed position.
    ///
    /// Does nothing unless the current transition is a restore.
    fn convert_restore_to_level_animation(&mut self, focal: ZoomTransitionFocal) {
        let level = self.level();
        // The displayed focal point is sampled before the restore is taken
        // apart: afterwards `focal()` would report the stale committed value.
        let displayed_focal = self.focal();
        let Some(ZoomLevelTransition::Restore {
            animation,
            from_level,
            to_level,
            clock,
            config,
            ..
        }) = self.transition.take()
        else {
            return;
        };

        // Zooming out to the identity transform degenerates the anchored
        // focal solve (it divides by `level - 1`), so an anchor would fly to
        // a clamped edge as the level approaches 1. Keep the current focal
        // point fixed instead, like `set_target_level` does for target 1.
        let focal = if to_level == 1. && matches!(focal, ZoomTransitionFocal::Anchored { .. }) {
            ZoomTransitionFocal::Fixed {
                focal: displayed_focal,
            }
        } else {
            focal
        };

        let dz = to_level.log2() - from_level.log2();
        let velocity = if dz.abs() > RESTORE_VELOCITY_MIN_DELTA {
            animation.velocity().unwrap_or(0.) * dz
        } else {
            0.
        };

        self.transition = Some(ZoomLevelTransition::Animation {
            animation: Animation::new(clock, level.log2(), to_level.log2(), velocity, config),
            focal,
            display_follow: None,
        });
    }

    /// The focal point that keeps `content` displayed at `display` for the
    /// given level, clamped to the output.
    ///
    /// Solves `display = focal + (content - focal) * level` for `focal`. At
    /// `level == 1` the transform is the identity and the focal point is
    /// unconstrained, so the committed focal point is kept.
    fn anchored_focal(
        &self,
        content: Point<f64, Logical>,
        display: Point<f64, Logical>,
        level: f64,
    ) -> Point<f64, Logical> {
        let d = level - 1.;
        if d == 0. {
            return Self::clamp_focal(self.focal, self.view_size);
        }

        let focal = Point::from((
            (level * content.x - display.x) / d,
            (level * content.y - display.y) / d,
        ));
        Self::clamp_focal(focal, self.view_size)
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

        state.set_locked(true);
        assert!(state.is_locked());

        // This cursor would move the focal point if not locked.
        let cursor = Point::from((1500., 540.));
        assert!(!track(&mut state, cursor, 0.5));
        assert_point_eq(state.focal(), Point::from((960., 540.)));

        state.toggle_locked();
        assert!(!state.is_locked());
        assert!(track(&mut state, cursor, 0.5));
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
        a.set_locked(true);

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
    fn transition_animation(state: &OutputZoomState) -> Option<&Animation> {
        match &state.transition {
            Some(ZoomLevelTransition::Animation { animation, .. })
            | Some(ZoomLevelTransition::Restore { animation, .. }) => Some(animation),
            Some(ZoomLevelTransition::Gesturing { .. })
            | Some(ZoomLevelTransition::Follow { .. })
            | None => None,
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
        state.set_locked(true);
        let mut clock = test_clock();

        // The content point at the viewport center: it must stay fixed while
        // the level animates, so the focal point moves.
        let center = view_size.to_point().downscale(2.);
        let center_content = state.viewport_transform().apply_inverse(center);

        state.set_target_level(4., Point::from((960., 540.)), &clock, test_anim_config());

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

        state.set_target_level(3., anchor, &clock, test_anim_config());
        advance(&mut state, &mut clock, 50);

        state.set_locked(true);
        let frozen = state.focal();
        advance(&mut state, &mut clock, 20);

        // Unlocking must not move the camera.
        state.set_locked(false);
        assert_point_eq(state.focal(), frozen);

        // Deadzone tracking waits for the level animation to finish.
        assert!(!track(&mut state, Point::from((0., 0.)), 0.));
        assert_point_eq(state.focal(), frozen);

        advance(&mut state, &mut clock, 5000);
        assert_eq!(state.level(), 3.);

        // Once the animation completes, the follow evaluation moves the
        // camera to the deadzone edge.
        assert!(track(&mut state, Point::from((0., 0.)), 0.));
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
        state.focal = saved_focal;
        let snapshot = snapshot_of(&state);
        assert_eq!(snapshot.target_level, 1.);

        // The temporary hold-zoom state: 2x around a different focal point.
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
            state.transition,
            Some(ZoomLevelTransition::Animation { .. })
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
    fn restore_lock_converts_to_fixed_animation() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((400., 300.)), view_size);
        let mut clock = test_clock();
        let snapshot = snapshot_of(&state);

        assert!(track(&mut state, Point::from((0., 0.)), 0.));
        // A slow easing restore so that it is still in flight when the lock
        // engages.
        state.restore_animated(snapshot, &clock, easing_config(1000));
        advance(&mut state, &mut clock, 50);

        // Locking mid-restore freezes the current focal point and discards
        // the saved destination.
        let frozen = state.focal();
        state.set_locked(true);
        assert!(matches!(
            state.transition,
            Some(ZoomLevelTransition::Animation {
                focal: ZoomTransitionFocal::Fixed { .. },
                ..
            })
        ));

        for _ in 0..10 {
            advance(&mut state, &mut clock, 20);
            assert_point_eq(state.focal(), frozen);
        }
        advance(&mut state, &mut clock, 5000);
        assert_eq!(state.level(), 2.);
        assert_point_eq(state.focal(), frozen);
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
        state.set_locked(true);
        let fixed = state.focal();

        let mut snapshot = snapshot;
        snapshot.target_level = 2.;
        state.restore_animated(snapshot, &clock, test_anim_config());
        assert!(state.is_animating());

        for _ in 0..10 {
            advance(&mut state, &mut clock, 20);
            assert_point_eq(state.focal(), fixed);
            let level = state.level();
            assert!(level >= 2. - EPS && level <= 3. + EPS);
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
            state.transition,
            Some(ZoomLevelTransition::Animation { .. })
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
        assert!(state.transition.is_none());
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
            assert!(level >= 1.5 && level <= 3., "level out of bounds: {level}");
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
        assert!(matches!(
            state.transition,
            Some(ZoomLevelTransition::Gesturing { start_level, .. })
                if start_level == displayed
        ));
        assert_abs_diff_eq!(state.level(), displayed, epsilon = EPS);
        assert_abs_diff_eq!(state.target_level(), displayed, epsilon = EPS);
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
        assert!(matches!(
            state.transition,
            Some(ZoomLevelTransition::Gesturing { .. })
        ));
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
        state.set_locked(true);
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
        state.set_locked(true);
        assert!(state.update_gesture(0.75, 10.));

        assert_eq!(state.level(), 1.5);
        assert_point_eq(state.focal(), frozen);
    }

    #[test]
    fn gesture_unlock_mid_gesture() {
        let view_size = Size::from((1920., 1080.));
        let mut state = state_at(2., Point::from((960., 540.)), view_size);

        state.begin_gesture(Point::from((700., 400.)));
        state.set_locked(true);
        assert!(state.update_gesture(1.5, 10.));
        let frozen = state.focal();

        // Unlocking does not jump back to the anchor: the fixed focal point
        // remains until the gesture ends.
        state.set_locked(false);
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
        assert!(state.transition.is_none());
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
        assert!(state.transition.is_none());
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
