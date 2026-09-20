use std::time::Duration;

use niri::layout::{ActivateWindow, AddWindowTarget, LayoutElement as _, Options, SizingMode};
use niri::render_helpers::solid_color::{SolidColorBuffer, SolidColorRenderElement};
use niri::render_helpers::{RenderCtx, RenderTarget};
use niri::window::ResolvedWindowRules;
use niri_config::{BorderRule, Color, CornerRadius, FloatOrInt, KnitBorderRule, OutputName};
use smithay::backend::renderer::element::{Kind, RenderElement};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::desktop::layer_map_for_output;
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::utils::{Logical, Physical, Point, Size};

use super::knit_settings::{KnitSettings, SharedKnitState};
use super::{Args, TestCase};
use crate::test_window::TestWindow;

/// Auto width oscillation period in seconds.
const AUTO_WIDTH_PERIOD: f64 = 5.;
/// Auto height oscillation period in seconds.
const AUTO_HEIGHT_PERIOD: f64 = 7.;
/// Auto corner radius oscillation period in seconds.
const AUTO_RADIUS_PERIOD: f64 = 4.;

/// Smooth min-max-min oscillation over a phase in `[0, 1)` periods.
fn oscillate(phase: f64) -> f64 {
    (1. - (phase * std::f64::consts::TAU).cos()) / 2.
}

/// Independent per-axis auto animation state.
///
/// `phase` only advances while the axis is enabled, so disabling freezes the
/// displayed value and idle time never accumulates into a jump. On re-enable
/// the phase is kept if it still produces the current value (seamless resume
/// preserving direction), otherwise it is re-solved on the rising branch so
/// motion starts from the current value without a jump.
#[derive(Debug, Default)]
struct AutoAxis {
    phase: f64,
    was_enabled: bool,
}

impl AutoAxis {
    fn advance(&mut self, enabled: bool, value: &mut f64, dt: f64, period: f64) {
        if !enabled {
            self.was_enabled = false;
            return;
        }

        if self.was_enabled {
            self.phase = (self.phase + dt / period).rem_euclid(1.);
        } else {
            self.was_enabled = true;
            let v = (*value / 100.).clamp(0., 1.);
            if (oscillate(self.phase) - v).abs() > 1e-9 {
                self.phase = (1. - 2. * v).acos() / std::f64::consts::TAU;
            }
        }

        *value = oscillate(self.phase) * 100.;
    }

    fn active(&self) -> bool {
        self.was_enabled
    }
}

/// Everything derived from settings and the view size for one application.
///
/// Compared against the last applied snapshot so unchanged frames don't
/// re-apply rules or re-request sizes.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Desired {
    view: Size<f64, Logical>,
    /// Effective border width after clamping to fit the viewport.
    border: f64,
    /// Effective margin after clamping to fit the viewport.
    margin: f64,
    /// Content size in logical pixels, `>= 1` on both axes.
    content: Size<i32, Logical>,
    /// Applied corner radius in logical pixels.
    radius_px: f64,
    rules: BorderRule,
    background: Color,
    inner: Color,
}

pub struct Knit {
    output: Output,
    window: TestWindow,
    layout: niri::layout::Layout<TestWindow>,
    state: SharedKnitState,
    view_size: Size<f64, Logical>,
    /// Extra margin around the window bounds, taken from `layout.gaps`.
    margin: f64,
    background_buffer: SolidColorBuffer,
    applied: Option<Desired>,
    auto_width: AutoAxis,
    auto_height: AutoAxis,
    auto_radius: AutoAxis,
    last_time: Option<Duration>,
}

impl Knit {
    pub fn new(args: Args, state: SharedKnitState) -> Self {
        let Args { size, clock } = args;
        let view_size = size.to_f64();

        let output = Output::new(
            String::new(),
            PhysicalProperties {
                size: Size::from((size.w, size.h)),
                subpixel: Subpixel::Unknown,
                make: String::new(),
                model: String::new(),
                serial_number: String::new(),
            },
        );
        let mode = Some(Mode {
            size: size.to_physical(1),
            refresh: 60000,
        });
        output.change_current_state(mode, None, None, None);
        output.user_data().insert_if_missing(|| OutputName {
            connector: String::new(),
            make: None,
            model: None,
            serial: None,
        });

        let mut options = Options {
            layout: niri_config::Layout {
                focus_ring: niri_config::FocusRing {
                    off: true,
                    ..Default::default()
                },
                border: niri_config::Border {
                    off: false,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        };
        // Centering goes through the window movement animation config; disable
        // it so re-centering on every resize is instant rather than animated.
        options.animations.window_movement.0.off = true;
        let margin = options.layout.gaps;

        let mut layout = niri::layout::Layout::with_options(clock, options);
        layout.add_output(output.clone(), None);

        let settings = state.borrow().settings;
        let desired = Self::compute_desired(&settings, view_size, margin);

        let mut window = TestWindow::freeform(0);
        window.set_rules(Self::rules(&desired, true));
        window.request_size(desired.content, SizingMode::Normal, false, None);
        window.communicate();

        layout.add_window(
            window.clone(),
            AddWindowTarget::Auto,
            None,
            None,
            false,
            true,
            ActivateWindow::default(),
        );

        let background_buffer =
            SolidColorBuffer::new(view_size, desired.background.to_array_premul());

        Self {
            output,
            window,
            layout,
            state,
            view_size,
            margin,
            background_buffer,
            applied: None,
            auto_width: AutoAxis::default(),
            auto_height: AutoAxis::default(),
            auto_radius: AutoAxis::default(),
            last_time: None,
        }
    }

    fn rules(desired: &Desired, open_floating: bool) -> ResolvedWindowRules {
        ResolvedWindowRules {
            border: desired.rules,
            geometry_corner_radius: Some(CornerRadius::from(desired.radius_px as f32)),
            open_floating: open_floating.then_some(true),
            ..Default::default()
        }
    }

    /// Maps settings and the view size to concrete window parameters.
    ///
    /// The window including its border and a margin (from `layout.gaps`) must
    /// fit the viewport. On tiny viewports the margin shrinks first, then the
    /// border, and the content never goes below 1px per axis.
    fn compute_desired(settings: &KnitSettings, view: Size<f64, Logical>, margin: f64) -> Desired {
        let half_space = ((view.w.min(view.h) - 1.) / 2.).max(0.);
        let border = settings.border_width.clamp(0., half_space).floor();
        let margin = margin.clamp(0., half_space - border);

        let avail_w = (view.w - 2. * (border + margin)).max(1.);
        let avail_h = (view.h - 2. * (border + margin)).max(1.);

        let w = (1. + (avail_w - 1.) * settings.width.clamp(0., 100.) / 100.)
            .round()
            .clamp(1., avail_w) as i32;
        let h = (1. + (avail_h - 1.) * settings.height.clamp(0., 100.) / 100.)
            .round()
            .clamp(1., avail_h) as i32;

        let max_radius = f64::from(w.min(h)) / 2.;
        let radius_px = max_radius * settings.radius.clamp(0., 100.) / 100.;

        let rules = BorderRule {
            on: border > 0.,
            off: border == 0.,
            width: Some(FloatOrInt(border)),
            active_color: Some(settings.base),
            inactive_color: Some(settings.base),
            knit: KnitBorderRule {
                on: settings.enabled,
                off: !settings.enabled,
                pattern: Some(settings.pattern),
                accent_color: Some(settings.accent),
                stitch_size: Some(FloatOrInt(settings.stitch_size.clamp(1., 64.))),
                relief: Some(FloatOrInt(settings.relief.clamp(0., 1.))),
                fuzz: Some(FloatOrInt(settings.fuzz.clamp(0., 1.))),
            },
            ..Default::default()
        };

        Desired {
            view,
            border,
            margin,
            content: Size::from((w, h)),
            radius_px,
            rules,
            background: settings.background,
            inner: settings.inner,
        }
    }

    /// Applies changed settings to the live layout window.
    ///
    /// Rule changes go through `set_rules` on the layout's own window handle
    /// plus `Layout::update_window`, which re-merges the border config and
    /// refreshes the tile. Size changes request a new size, communicate it and
    /// update the window. Anything that can change the tile size re-centers
    /// the floating window (instantly, since window movement is off).
    fn apply(&mut self, desired: Desired) {
        let prev = self.applied;
        if prev == Some(desired) {
            return;
        }

        let rules_changed =
            prev.is_none_or(|p| p.rules != desired.rules || p.radius_px != desired.radius_px);
        let size_changed = prev.is_none_or(|p| p.content != desired.content);
        let view_changed = prev.is_none_or(|p| p.view != desired.view);

        if rules_changed {
            let rules = Self::rules(&desired, false);
            self.window.set_rules(rules.clone());
            if let Some(ws) = self.layout.active_workspace_mut() {
                for win in ws.windows_mut() {
                    win.set_rules(rules.clone());
                }
            }
        }

        self.window
            .request_size(desired.content, SizingMode::Normal, false, None);
        let resized = self.window.communicate();

        if rules_changed || resized {
            self.layout.update_window(self.window.id(), None);
        }

        if rules_changed || size_changed || view_changed {
            self.layout.center_window(Some(self.window.id()));
        }

        if prev.is_none_or(|p| p.inner != desired.inner) {
            self.window.set_color(desired.inner.to_array_premul());
        }
        self.background_buffer
            .update(desired.view, desired.background.to_array_premul());

        self.applied = Some(desired);
    }
}

impl TestCase for Knit {
    fn resize(&mut self, width: i32, height: i32) {
        let mode = Some(Mode {
            size: Size::from((width, height)),
            refresh: 60000,
        });
        self.output.change_current_state(mode, None, None, None);
        layer_map_for_output(&self.output).arrange();
        self.layout.update_output_size(&self.output);
        self.view_size = Size::from((width, height)).to_f64();
    }

    fn are_animations_ongoing(&self) -> bool {
        self.auto_width.active()
            || self.auto_height.active()
            || self.auto_radius.active()
            || self.layout.are_animations_ongoing(Some(&self.output))
    }

    fn advance_animations(&mut self, current_time: Duration) {
        self.layout.advance_animations();

        let dt = self
            .last_time
            .map(|last| current_time.saturating_sub(last).as_secs_f64())
            .unwrap_or(0.);
        self.last_time = Some(current_time);

        {
            let mut state = self.state.borrow_mut();
            let settings = &mut state.settings;
            self.auto_width.advance(
                settings.auto_width,
                &mut settings.width,
                dt,
                AUTO_WIDTH_PERIOD,
            );
            self.auto_height.advance(
                settings.auto_height,
                &mut settings.height,
                dt,
                AUTO_HEIGHT_PERIOD,
            );
            self.auto_radius.advance(
                settings.auto_radius,
                &mut settings.radius,
                dt,
                AUTO_RADIUS_PERIOD,
            );
        }

        let settings = self.state.borrow().settings;
        let desired = Self::compute_desired(&settings, self.view_size, self.margin);
        self.apply(desired);

        let mut state = self.state.borrow_mut();
        state.window_size = (self.window.size().w, self.window.size().h);
        state.corner_radius = desired.radius_px;
    }

    fn render(
        &mut self,
        renderer: &mut GlesRenderer,
        _size: Size<i32, Physical>,
    ) -> Vec<Box<dyn RenderElement<GlesRenderer>>> {
        self.layout.update_render_elements(Some(&self.output));

        let mut rv: Vec<Box<dyn RenderElement<GlesRenderer>>> = Vec::new();
        let ctx = RenderCtx {
            renderer,
            target: RenderTarget::Output,
            xray: None,
        };
        self.layout
            .monitor_for_output(&self.output)
            .unwrap()
            .render_workspaces(ctx, true, &mut |elem| rv.push(Box::new(elem) as _));

        // Elements draw in reverse order, so the background goes last.
        rv.push(Box::new(SolidColorRenderElement::from_buffer(
            &self.background_buffer,
            Point::new(0., 0.),
            1.,
            Kind::Unspecified,
        )));
        rv
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use niri::animation::Clock;
    use smithay::utils::Size;

    use super::*;
    use crate::cases::knit_settings::KnitState;

    fn make(size: (i32, i32)) -> (Knit, SharedKnitState, Clock) {
        let clock = Clock::with_time(Duration::ZERO);
        let state: SharedKnitState = Rc::new(RefCell::new(KnitState::default()));
        let knit = Knit::new(
            Args {
                size: Size::from(size),
                clock: clock.clone(),
            },
            state.clone(),
        );
        (knit, state, clock)
    }

    fn advance(knit: &mut Knit, clock: &Clock, time: Duration) {
        clock.clone().set_unadjusted(time);
        knit.advance_animations(clock.now());
    }

    #[test]
    fn auto_freezes_and_resumes_without_jump() {
        let (mut knit, state, clock) = make((800, 600));
        state.borrow_mut().settings.auto_width = true;

        advance(&mut knit, &clock, Duration::ZERO);
        advance(&mut knit, &clock, Duration::from_millis(500));
        let frozen = state.borrow().settings.width;

        state.borrow_mut().settings.auto_width = false;
        advance(&mut knit, &clock, Duration::from_secs(10));
        assert_eq!(state.borrow().settings.width, frozen);

        state.borrow_mut().settings.auto_width = true;
        advance(&mut knit, &clock, Duration::from_secs(11));
        let resumed = state.borrow().settings.width;
        assert!((resumed - frozen).abs() < 1.);

        advance(&mut knit, &clock, Duration::from_secs(12));
        assert_ne!(state.borrow().settings.width, resumed);
    }

    #[test]
    fn auto_axes_run_independently() {
        let (mut knit, state, clock) = make((800, 600));
        state.borrow_mut().settings.auto_width = true;
        state.borrow_mut().settings.auto_height = true;

        advance(&mut knit, &clock, Duration::ZERO);
        advance(&mut knit, &clock, Duration::from_secs(1));

        let w = state.borrow().settings.width;
        let h = state.borrow().settings.height;
        // 1s into 5s and 7s cosine periods, rising from the 65% default.
        assert!((w - 65.).abs() > 1.);
        assert!((h - 65.).abs() > 1.);
        assert!((w - h).abs() > 1.);

        // Radius stays put: its auto flag is off.
        assert_eq!(state.borrow().settings.radius, 20.);
    }

    #[test]
    fn tiny_viewport_keeps_window_inside() {
        let (mut knit, state, clock) = make((30, 20));
        advance(&mut knit, &clock, Duration::ZERO);

        let (w, h) = state.borrow().window_size;
        assert!(w >= 1 && h >= 1);
        // Border and margin are clamped so content + border + margin fits.
        let applied = knit.applied.unwrap();
        assert!(f64::from(w) + 2. * (applied.border + applied.margin) <= 30. + 1e-9);
        assert!(f64::from(h) + 2. * (applied.border + applied.margin) <= 20. + 1e-9);
    }

    #[test]
    fn percent_maps_to_available_bounds() {
        let (mut knit, state, clock) = make((800, 600));
        advance(&mut knit, &clock, Duration::ZERO);

        // 65% of 800 - 2 * (48 + 16) = 672 available.
        assert_eq!(state.borrow().window_size, (437, 307));

        state.borrow_mut().settings.width = 0.;
        state.borrow_mut().settings.height = 100.;
        advance(&mut knit, &clock, Duration::from_millis(16));
        assert_eq!(state.borrow().window_size, (1, 472));
    }
}
