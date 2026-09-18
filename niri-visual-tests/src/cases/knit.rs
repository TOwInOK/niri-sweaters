use std::time::Duration;

use niri::layout::{ActivateWindow, AddWindowTarget, LayoutElement as _, Options, SizingMode};
use niri::render_helpers::{RenderCtx, RenderTarget};
use niri::window::ResolvedWindowRules;
use niri_config::{
    BorderRule, Color, CornerRadius, FloatOrInt, KnitBorder, KnitBorderRule, KnitPattern,
    OutputName, PresetSize,
};
use smithay::backend::renderer::element::RenderElement;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::desktop::layer_map_for_output;
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::utils::{Physical, Size};

use super::{Args, TestCase};
use crate::test_window::TestWindow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopKind {
    None,
    /// Corner radius oscillates between fully rounded and zero on every window.
    Radius,
    /// The window itself stretches and shrinks horizontally.
    Resize,
}

pub struct Knit {
    output: Output,
    windows: Vec<TestWindow>,
    layout: niri::layout::Layout<TestWindow>,
    loop_kind: LoopKind,
    patterns: Vec<KnitPattern>,
}

fn rules(pattern: KnitPattern, radius: f64) -> ResolvedWindowRules {
    ResolvedWindowRules {
        border: BorderRule {
            knit: KnitBorderRule {
                on: true,
                pattern: Some(pattern),
                accent_color: Some(Color::from_rgba8_unpremul(0x8f, 0xa6, 0xbf, 0xff)),
                stitch_size: Some(FloatOrInt(8.)),
                relief: Some(FloatOrInt(0.65)),
                fuzz: Some(FloatOrInt(0.15)),
                ..Default::default()
            },
            ..Default::default()
        },
        geometry_corner_radius: Some(CornerRadius::from(radius as f32)),
        ..Default::default()
    }
}

impl Knit {
    fn new(args: Args, border_width: f64, pattern: KnitPattern) -> Self {
        let Args { size, clock } = args;

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

        let options = Options {
            layout: niri_config::Layout {
                focus_ring: niri_config::FocusRing {
                    off: true,
                    ..Default::default()
                },
                border: niri_config::Border {
                    off: false,
                    width: border_width,
                    active_color: Color::from_rgba8_unpremul(0x52, 0x6c, 0x89, 0xff),
                    inactive_color: Color::from_rgba8_unpremul(0x52, 0x6c, 0x89, 0xff),
                    urgent_color: Color::from_rgba8_unpremul(155, 0, 0, 255),
                    active_gradient: None,
                    inactive_gradient: None,
                    urgent_gradient: None,
                    knit: KnitBorder {
                        off: false,
                        pattern,
                        accent_color: Color::from_rgba8_unpremul(0x8f, 0xa6, 0xbf, 0xff),
                        stitch_size: 8.,
                        relief: 0.65,
                        fuzz: 0.15,
                    },
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let mut layout = niri::layout::Layout::with_options(clock, options);
        layout.add_output(output.clone(), None);

        Self {
            output,
            windows: Vec::new(),
            layout,
            loop_kind: LoopKind::None,
            patterns: Vec::new(),
        }
    }

    pub fn patterns(args: Args) -> Self {
        let mut rv = Self::new(args, 48., KnitPattern::Stockinette);
        rv.loop_kind = LoopKind::Radius;

        for (i, pattern) in [
            KnitPattern::Stockinette,
            KnitPattern::Rib,
            KnitPattern::Checker,
            KnitPattern::Zigzag,
            KnitPattern::Diamond,
            KnitPattern::Dots,
        ]
        .into_iter()
        .enumerate()
        {
            let mut win = TestWindow::freeform(i);
            win.set_rules(rules(pattern, 0.));
            rv.add_window(win, Some(PresetSize::Proportion(1. / 6.)));
            rv.patterns.push(pattern);
        }

        rv
    }

    pub fn rounded_corners(args: Args) -> Self {
        let mut rv = Self::new(args, 30., KnitPattern::Checker);

        for (i, radius) in [24., 6.].into_iter().enumerate() {
            let mut win = TestWindow::freeform(i);
            win.set_rules(rules(KnitPattern::Checker, radius));
            rv.add_window(win, Some(PresetSize::Proportion(0.5)));
        }

        rv
    }

    pub fn resize_rounded(args: Args) -> Self {
        let mut rv = Self::new(args, 30., KnitPattern::Checker);
        rv.loop_kind = LoopKind::Resize;

        let mut win = TestWindow::freeform(0);
        let mut window_rules = rules(KnitPattern::Checker, 24.);
        // Floating so the border hugs the window and stretches together with it.
        window_rules.open_floating = Some(true);
        win.set_rules(window_rules);
        rv.add_window(win, None);

        rv
    }

    fn add_window(&mut self, mut window: TestWindow, width: Option<PresetSize>) {
        let ws = self.layout.active_workspace().unwrap();
        let min_size = window.min_size();
        let max_size = window.max_size();
        window.request_size(
            ws.new_window_size(width, None, false, window.rules(), (min_size, max_size)),
            SizingMode::Normal,
            false,
            None,
        );
        window.communicate();

        self.layout.add_window(
            window.clone(),
            AddWindowTarget::Auto,
            width,
            None,
            false,
            false,
            ActivateWindow::default(),
        );
        self.windows.push(window);
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
        for win in &self.windows {
            if win.communicate() {
                self.layout.update_window(win.id(), None);
            }
        }
    }

    fn are_animations_ongoing(&self) -> bool {
        self.loop_kind != LoopKind::None || self.layout.are_animations_ongoing(Some(&self.output))
    }

    fn advance_animations(&mut self, current_time: Duration) {
        const TAU: f64 = std::f64::consts::TAU;

        self.layout.advance_animations();

        match self.loop_kind {
            LoopKind::None => (),
            LoopKind::Radius => {
                // Oscillate every window's corner radius between zero and fully
                // rounded (half the smaller side, i.e. a capsule), with a phase
                // offset per window so the loop is visible at a glance. The
                // outer border contour follows concentrically via expanded_by.
                let t = current_time.as_secs_f64();
                let Some(ws) = self.layout.active_workspace_mut() else {
                    return;
                };
                for (i, win) in ws.windows_mut().enumerate() {
                    let pattern = self.patterns.get(i).copied().unwrap_or_default();
                    let phase = i as f64 * TAU / self.patterns.len().max(1) as f64;
                    let size = win.size().to_f64();
                    let max_radius = size.w.min(size.h) / 2.;
                    let radius = max_radius * (1. - (t * TAU / 4. + phase).cos()) / 2.;
                    win.set_rules(rules(pattern, radius));
                }
            }
            LoopKind::Resize => {
                // Stretch the floating window horizontally; the border hugs it
                // and the knit band stretches together with the window.
                let t = current_time.as_secs_f64();
                let fraction = 0.5 + 0.35 * (t * TAU / 5.).sin();
                let Some(ws) = self.layout.active_workspace_mut() else {
                    return;
                };
                let view = ws.view_size();
                let Some(win) = ws.windows_mut().next() else {
                    return;
                };
                win.request_size(
                    Size::from(((view.w * fraction) as i32, (view.h * 0.6) as i32)),
                    SizingMode::Normal,
                    false,
                    None,
                );
                if self.windows[0].communicate() {
                    self.layout.update_window(self.windows[0].id(), None);
                }
            }
        }
    }

    fn render(
        &mut self,
        renderer: &mut GlesRenderer,
        _size: Size<i32, Physical>,
    ) -> Vec<Box<dyn RenderElement<GlesRenderer>>> {
        self.layout.update_render_elements(Some(&self.output));

        let mut rv = Vec::new();
        let ctx = RenderCtx {
            renderer,
            target: RenderTarget::Output,
            xray: None,
        };
        self.layout
            .monitor_for_output(&self.output)
            .unwrap()
            .render_workspaces(ctx, true, &mut |elem| rv.push(Box::new(elem) as _));
        rv
    }
}
