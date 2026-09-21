//! Benchmark for focus ring and knit/gradient border rendering in a headless GlesRenderer.
//!
//! Renders six focus rings (48 draw elements per frame) into an offscreen texture
//! (3288x1424, Fourcc::Abgr8888), measures per-frame wall time with confirmed GPU
//! synchronization (EGL fences or `glFinish`), and optionally dumps frames as PNG.
//! Two workloads are available: `static` redraws identical geometry every frame,
//! `resize` recomputes ring geometry each frame along a min → max → min window
//! resize cycle.
//!
//! # CLI Usage
//!
//! ```text
//! cargo bench --locked -p niri --bench border_bench [-- <args>]
//! ```
//!
//! Note: Arguments intended for the benchmark binary must follow `--`.
//!
//! ## Options
//!
//! - `--scenario <name|all>`: Scenario to execute. Can be specified multiple times to run a subset.
//!   Defaults to `all`. Available scenarios:
//!   - `solid`: Solid color border without gradients or knit patterns.
//!   - `gradient-srgb`: sRGB color space window-relative gradient.
//!   - `gradient-oklch`: Oklch color space gradient with longer hue interpolation.
//!   - `knit-stockinette`: Stockinette knit pattern (stitch size 8, fuzz 0.4).
//!   - `knit-zigzag`: Zigzag knit pattern (stitch size 8, fuzz 0.4).
//!   - `knit-zigzag-fuzz`: Zigzag knit pattern with heavy fuzz (stitch size 8, fuzz 0.8).
//!   - `knit-zigzag-detail`: Large zigzag knit pattern (stitch size 32, fuzz 0.8).
//!   - `knit-gradient-stockinette`: Stockinette knit over an sRGB gradient.
//!   - `knit-gradient-rib`: Rib knit over an sRGB gradient.
//!   - `knit-gradient-checker`: Checker knit over an sRGB gradient.
//!   - `knit-gradient-zigzag`: Zigzag knit over an sRGB gradient.
//!   - `knit-gradient-diamond`: Diamond knit over an sRGB gradient.
//!   - `knit-gradient-dots`: Dots knit over an sRGB gradient.
//!   - `all`: Runs all thirteen scenarios in order.
//!
//! - `--workload <name|all>`: Workload to execute per scenario. Can be specified multiple times.
//!   Defaults to `static`. Available workloads:
//!   - `static`: Elements are prepared once; every frame redraws identical geometry. Measures
//!     render + clear + finish + completion wait.
//!   - `resize`: Each frame advances one step of a 60-step resize cycle (window contents 210x648 →
//!     420x1296 → 210x648, cosine easing, sizes rounded to physical pixels). Measures
//!     `FocusRing::update_render_elements`
//!     + element collection + the full render path. `--frames` must be a
//!     multiple of 60 so the series covers whole cycles. `--warmup` counts
//!     cycle steps and needs no alignment. In `--smoke` mode one full cycle
//!     (60 frames) is drawn unmeasured.
//!   - `all`: Runs both workloads per scenario.
//!
//! - `--dump-dir`: Saves the final rendered frame of each scenario as `<scenario>.png` into
//!   `target/border_bench/`. For the resize workload, saves the cycle extremes as
//!   `<scenario>-resize-min.png` and `<scenario>-resize-max.png`.
//!
//! - `--warmup <N>`: Number of unmeasured warmup frames per scenario (default: `30`). Cannot be
//!   combined with `--smoke`.
//!
//! - `--runs <N>`: Repetitions of the whole scenario/workload series (default: `1`). With N > 1,
//!   reported statistics are means of per-run statistics (min/median/mean/p95/max), `samples_us`
//!   pools all runs, and each result carries a `runs` array with per-run raw data. A `Total /
//!   Summary` block is printed after the table. Can be combined with `--smoke` (each smoke run is
//!   repeated N times).
//!
//! - `--frames <N>`: Number of measured frames per scenario (default: `300`). Cannot be combined
//!   with `--smoke`.
//!
//! - `--smoke`: Runs exactly 1 unmeasured frame per scenario to verify shader compilation, geometry
//!   bounds, and GPU synchronization without collecting statistics. Cannot be combined with
//!   `--warmup` or `--frames`.
//!
//! - `--json`: Emits a single JSON document on stdout (`schema_version: 3`). All informational logs
//!   and diagnostic messages are redirected to stderr.
//!
//! # Common Examples
//!
//! - Run the standard benchmark suite: ```bash cargo bench --locked -p niri --bench border_bench
//!   ```
//!
//! - Fast smoke check (verify rendering pipelines and shaders): ```bash cargo bench --locked -p
//!   niri --bench border_bench -- --smoke ```
//!
//! - Dump rendered scenario images for visual inspection: ```bash cargo bench --locked -p niri
//!   --bench border_bench -- --smoke --dump-dir ```
//!
//! - Benchmark a specific scenario with custom frame counts: ```bash cargo bench --locked -p niri
//!   --bench border_bench -- --scenario knit-zigzag --warmup 20 --frames 200 ```
//!
//! - Export machine-readable JSON results to a file: ```bash cargo bench --locked -p niri --bench
//!   border_bench -- --json > report.json ```
//!
//! # Visual Regression Testing (Golden Images)
//!
//! When performing optimizations on borders, shaders, or knit patterns, always verify
//! that the visual output matches the golden reference images:
//!
//! ```bash
//! cargo test knit
//! ```
//!
//! If your system defaults to a hardware EGL vendor (such as NVIDIA) and the test
//! requires Mesa's deterministic `llvmpipe` software renderer, force Mesa EGL:
//! ```bash
//! __EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json cargo test knit
//! ```
//!
//! ## Mismatch Detection and `.new.png` Artifacts
//!
//! If any golden test fails (pixel difference exceeds 1%):
//! - The test writes the newly rendered frame to `src/tests/golden/<test_name>.new.png`.
//! - Compare `src/tests/golden/<test_name>.png` (golden reference) against
//!   `src/tests/golden/<test_name>.new.png` to inspect the visual discrepancy.
//! - If the change is intentional across patterns, regenerate the golden images: ```bash
//!   NIRI_GOLDEN_UPDATE=1 cargo test knit ```
//!
//! # JSON Output Schema (`schema_version: 3`)
//!
//! When `--json` is specified, stdout outputs a single JSON object with the following schema:
//!
//! ```json
//! {
//!   "schema_version": 3,
//!   "methodology_version": 3,
//!   "metadata": {
//!     "gl_vendor": "string | null",
//!     "gl_renderer": "string | null",
//!     "gl_version": "string | null",
//!     "package_version": "string",
//!     "build_version": "string",
//!     "debug_assertions": false,
//!     "target_size": [3288, 1424],
//!     "target_format": "Abgr8888",
//!     "window_count": 6,
//!     "window_content_size": [420.0, 1296.0],
//!     "border_width": 64.0,
//!     "outer_corner_radius": 84.0,
//!     "scale": 1.0,
//!     "alpha": 1.0,
//!     "element_draws_per_frame": 48,
//!     "mode": "benchmark | smoke",
//!     "warmup_frames": 30,
//!     "measured_frames": 300,
//!     "scenarios": ["solid", "gradient-srgb", ...],
//!     "revision_source": "runtime_checkout",
//!     "checkout_head": "string | null",
//!     "checkout_dirty": "boolean | null",
//!     "workloads": ["static", "resize"],
//!     "resize": {
//!       "min_window_size": [210.0, 648.0],
//!       "max_window_size": [420.0, 1296.0],
//!       "cycle_steps": 60,
//!       "easing": "cosine"
//!     }
//!   },
//!   "results": [
//!     {
//!       "name": "solid",
//!       "workload": "static",
//!       "metric": "frame_wall_time_us",
//!       "scope": "renderer.render() + clear + element_draws_per_frame RenderElement::draw calls + finish + completion wait",
//!       "parameters": {
//!         "focus_ring": {
//!           "off": false,
//!           "width": 64.0,
//!           "active_color": [0.66, 0.28, 0.16, 1.0],
//!           "inactive_color": [0.66, 0.28, 0.16, 1.0],
//!           "urgent_color": [0.61, 0.0, 0.0, 1.0],
//!           "active_gradient": null,
//!           "inactive_gradient": null,
//!           "urgent_gradient": null
//!         },
//!         "knit": null
//!       },
//!       "observed_synchronization": {
//!         "frames_with_fence": 300,
//!         "frames_without_fence": 0
//!       },
//!       "samples_us": [123.45, 118.2, ...],
//!       "statistics": {
//!         "count": 300,
//!         "min_us": 110.5,
//!         "median_us": 121.3,
//!         "mean_us": 122.1,
//!         "p95_us": 135.0,
//!         "max_us": 150.2
//!       },
//!       "stages": null
//!     }
//!   ]
//! }
//! ```
//!
//! For `workload: "resize"`, `scope` additionally covers
//! `FocusRing::update_render_elements` + element collection, and `stages`
//! carries the per-stage breakdown:
//!
//! ```json
//! "stages": {
//!   "update_us": [12.3, ...],
//!   "render_us": [110.2, ...],
//!   "update_statistics": {"count": 300, ...},
//!   "render_statistics": {"count": 300, ...}
//! }
//! ```
//!
//! In `--smoke` mode, `samples_us` is empty `[]` and `statistics` is `null`.

use std::ffi::CStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context as _};
use niri::backend::Headless;
use niri::layout::focus_ring::{FocusRing, FocusRingRenderElement};
use niri::render_helpers::border::BorderRenderElement;
use niri::render_helpers::{copy_framebuffer, create_texture};
use niri::utils::write_png_rgba8;
use niri_config::{
    Color, CornerRadius, Gradient, GradientColorSpace, GradientInterpolation, GradientRelativeTo,
    HueInterpolation, KnitBorder, KnitPattern,
};
use serde::Serialize;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::{Element as _, RenderElement};
use smithay::backend::renderer::gles::{ffi, GlesRenderer, GlesTarget, GlesTexture};
use smithay::backend::renderer::{Bind as _, ExportMem as _, Frame as _, Renderer};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform};

/// Render target size in physical pixels.
const TARGET_SIZE: (i32, i32) = (3288, 1424);
/// Window contents size; each ring adds `BORDER_WIDTH` on every side.
const WIN_SIZE: (f64, f64) = (420., 1296.);
const BORDER_WIDTH: f64 = 64.;
/// Outer corner radius of the ring: window radius 20 + border width 64.
const OUTER_RADIUS: f32 = 84.;
const RING_COUNT: usize = 6;
/// Horizontal distance between window contents origins.
const RING_STEP: f64 = 548.;
/// Render scale and element alpha used for the whole series.
const SCALE: f64 = 1.;
const ALPHA: f32 = 1.;
/// Render elements produced by one `FocusRing` (8 border segments).
const ELEMENTS_PER_RING: usize = 8;
/// Render target pixel format.
const TARGET_FORMAT: Fourcc = Fourcc::Abgr8888;
/// Window contents size at the smallest point of the resize cycle. The
/// largest point is `WIN_SIZE`, so the ring always stays inside its slot.
const RESIZE_MIN_SIZE: (f64, f64) = (210., 648.);
/// Number of size transitions in one full resize cycle: min → max → min.
/// Even, so the maximum lands exactly on step `RESIZE_CYCLE_STEPS / 2`.
const RESIZE_CYCLE_STEPS: usize = 60;

/// Frame workload: a fixed-size scene or a window resize animation cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Workload {
    /// Elements are prepared once; every frame redraws identical geometry.
    Static,
    /// Every frame recomputes ring geometry for the next size in the cycle.
    Resize,
}

impl Workload {
    const ALL: [Workload; 2] = [Workload::Static, Workload::Resize];

    fn name(self) -> &'static str {
        match self {
            Workload::Static => "static",
            Workload::Resize => "resize",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|w| w.name() == name)
    }
}

/// Progress of the resize cycle at `step` transitions from the minimum:
/// cosine easing 0 → 1 → 0 over `RESIZE_CYCLE_STEPS` steps. Step 0 and step
/// `RESIZE_CYCLE_STEPS` both return exactly 0 (minimum size), step
/// `RESIZE_CYCLE_STEPS / 2` returns exactly 1 (maximum size).
fn resize_progress(step: usize) -> f64 {
    let phase = (step % RESIZE_CYCLE_STEPS) as f64 / RESIZE_CYCLE_STEPS as f64;
    (1. - (2. * std::f64::consts::PI * phase).cos()) / 2.
}

/// Window contents size at `step` transitions into the resize cycle,
/// interpolated between `RESIZE_MIN_SIZE` and `WIN_SIZE` and rounded to
/// physical pixels like `Tile::animated_window_size` does.
fn resize_win_size(step: usize) -> Size<f64, Logical> {
    let p = resize_progress(step);
    let w = RESIZE_MIN_SIZE.0 + (WIN_SIZE.0 - RESIZE_MIN_SIZE.0) * p;
    let h = RESIZE_MIN_SIZE.1 + (WIN_SIZE.1 - RESIZE_MIN_SIZE.1) * p;
    Size::from((w, h))
        .to_physical_precise_round(SCALE)
        .to_logical(SCALE)
}

fn base_color() -> Color {
    Color::from_rgba8_unpremul(0xa9, 0x47, 0x28, 0xff)
}

fn gradient_to() -> Color {
    Color::from_rgba8_unpremul(0x6d, 0x9d, 0xc5, 0xff)
}

fn knit_accent() -> Color {
    Color::from_rgba8_unpremul(0xf3, 0xd5, 0xa5, 0xff)
}

/// Fixed rendering scenarios.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scenario {
    Solid,
    GradientSrgb,
    GradientOklch,
    KnitStockinette,
    KnitZigzag,
    KnitZigzagFuzz,
    KnitZigzagDetail,
    KnitGradientStockinette,
    KnitGradientRib,
    KnitGradientChecker,
    KnitGradientZigzag,
    KnitGradientDiamond,
    KnitGradientDots,
}

impl Scenario {
    const ALL: [Scenario; 13] = [
        Scenario::Solid,
        Scenario::GradientSrgb,
        Scenario::GradientOklch,
        Scenario::KnitStockinette,
        Scenario::KnitZigzag,
        Scenario::KnitZigzagFuzz,
        Scenario::KnitZigzagDetail,
        Scenario::KnitGradientStockinette,
        Scenario::KnitGradientRib,
        Scenario::KnitGradientChecker,
        Scenario::KnitGradientZigzag,
        Scenario::KnitGradientDiamond,
        Scenario::KnitGradientDots,
    ];

    fn name(self) -> &'static str {
        match self {
            Scenario::Solid => "solid",
            Scenario::GradientSrgb => "gradient-srgb",
            Scenario::GradientOklch => "gradient-oklch",
            Scenario::KnitStockinette => "knit-stockinette",
            Scenario::KnitZigzag => "knit-zigzag",
            Scenario::KnitZigzagFuzz => "knit-zigzag-fuzz",
            Scenario::KnitZigzagDetail => "knit-zigzag-detail",
            Scenario::KnitGradientStockinette => "knit-gradient-stockinette",
            Scenario::KnitGradientRib => "knit-gradient-rib",
            Scenario::KnitGradientChecker => "knit-gradient-checker",
            Scenario::KnitGradientZigzag => "knit-gradient-zigzag",
            Scenario::KnitGradientDiamond => "knit-gradient-diamond",
            Scenario::KnitGradientDots => "knit-gradient-dots",
        }
    }
    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }
}

/// Per-scenario border configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ScenarioConfig {
    focus_ring: niri_config::FocusRing,
    knit: Option<KnitBorder>,
}

/// Builds the configuration for a scenario. This is the same factory used to
/// create the `FocusRing`s for rendering.
fn scenario_config(scenario: Scenario) -> ScenarioConfig {
    let mut focus_ring = niri_config::FocusRing {
        off: false,
        width: BORDER_WIDTH,
        active_color: base_color(),
        inactive_color: base_color(),
        urgent_color: Color::from_rgba8_unpremul(155, 0, 0, 255),
        active_gradient: None,
        inactive_gradient: None,
        urgent_gradient: None,
    };

    let gradient = |color_space, hue_interpolation| Gradient {
        from: base_color(),
        to: gradient_to(),
        angle: 90,
        relative_to: GradientRelativeTo::Window,
        in_: GradientInterpolation {
            color_space,
            hue_interpolation,
        },
    };

    let knit = |pattern, stitch_size, fuzz| KnitBorder {
        off: false,
        pattern,
        accent_color: knit_accent(),
        stitch_size,
        relief: 0.8,
        fuzz,
    };

    let knit = match scenario {
        Scenario::Solid => None,
        Scenario::GradientSrgb => {
            focus_ring.active_gradient = Some(gradient(
                GradientColorSpace::Srgb,
                HueInterpolation::Shorter,
            ));
            None
        }
        Scenario::GradientOklch => {
            focus_ring.active_gradient = Some(gradient(
                GradientColorSpace::Oklch,
                HueInterpolation::Longer,
            ));
            None
        }
        Scenario::KnitStockinette => Some(knit(KnitPattern::Stockinette, 8., 0.4)),
        Scenario::KnitZigzag => Some(knit(KnitPattern::Zigzag, 8., 0.4)),
        Scenario::KnitZigzagFuzz => Some(knit(KnitPattern::Zigzag, 8., 0.8)),
        Scenario::KnitZigzagDetail => Some(knit(KnitPattern::Zigzag, 32., 0.8)),
        Scenario::KnitGradientStockinette => {
            focus_ring.active_gradient = Some(gradient(
                GradientColorSpace::Srgb,
                HueInterpolation::Shorter,
            ));
            Some(knit(KnitPattern::Stockinette, 8., 0.4))
        }
        Scenario::KnitGradientRib => {
            focus_ring.active_gradient = Some(gradient(
                GradientColorSpace::Srgb,
                HueInterpolation::Shorter,
            ));
            Some(knit(KnitPattern::Rib, 8., 0.4))
        }
        Scenario::KnitGradientChecker => {
            focus_ring.active_gradient = Some(gradient(
                GradientColorSpace::Srgb,
                HueInterpolation::Shorter,
            ));
            Some(knit(KnitPattern::Checker, 8., 0.4))
        }
        Scenario::KnitGradientZigzag => {
            focus_ring.active_gradient = Some(gradient(
                GradientColorSpace::Srgb,
                HueInterpolation::Shorter,
            ));
            Some(knit(KnitPattern::Zigzag, 8., 0.4))
        }
        Scenario::KnitGradientDiamond => {
            focus_ring.active_gradient = Some(gradient(
                GradientColorSpace::Srgb,
                HueInterpolation::Shorter,
            ));
            Some(knit(KnitPattern::Diamond, 8., 0.4))
        }
        Scenario::KnitGradientDots => {
            focus_ring.active_gradient = Some(gradient(
                GradientColorSpace::Srgb,
                HueInterpolation::Shorter,
            ));
            Some(knit(KnitPattern::Dots, 8., 0.4))
        }
    };

    ScenarioConfig { focus_ring, knit }
}

// ---------------------------------------------------------------------------
// Report model (JSON output, schema_version 1)
// ---------------------------------------------------------------------------

/// Top-level report document emitted by `--json`.
#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    methodology_version: u32,
    metadata: Metadata,
    results: Vec<ScenarioResult>,
}

/// Fixed parameters of the resize workload, reported so results can be
/// interpreted without reading the benchmark source.
#[derive(Debug, Serialize)]
struct ResizeParams {
    /// Window contents size at the cycle minimum, logical pixels.
    min_window_size: [f64; 2],
    /// Window contents size at the cycle maximum, logical pixels.
    max_window_size: [f64; 2],
    /// Size transitions per full cycle (min → max → min).
    cycle_steps: usize,
    /// Easing curve name; progress is `(1 - cos(2π·t)) / 2`.
    easing: &'static str,
}

/// Environment and workload description collected once per run, outside the
/// measured series.
#[derive(Debug, Serialize)]
struct Metadata {
    gl_vendor: Option<String>,
    gl_renderer: Option<String>,
    gl_version: Option<String>,
    package_version: &'static str,
    /// Build-time version string from `niri::utils::version()`; describes the
    /// revision embedded when the binary was compiled.
    build_version: String,
    /// The `cfg!(debug_assertions)` flag itself; it does not prove a
    /// particular Cargo profile.
    debug_assertions: bool,
    target_size: [i32; 2],
    target_format: String,
    window_count: usize,
    window_content_size: [f64; 2],
    border_width: f64,
    outer_corner_radius: f32,
    scale: f64,
    alpha: f32,
    /// Number of `RenderElement::draw` calls issued per frame. This is not a
    /// measured count of low-level GL draw calls.
    element_draws_per_frame: usize,
    mode: &'static str,
    warmup_frames: usize,
    measured_frames: usize,
    scenarios: Vec<&'static str>,
    /// Origin of the checkout fields: "runtime_checkout" means they describe
    /// the working tree at run time, not necessarily the revision the binary
    /// was built from.
    revision_source: &'static str,
    /// HEAD of the working tree at run time; `null` when unavailable.
    checkout_head: Option<String>,
    /// Whether the working tree had tracked or untracked changes at run time;
    /// `null` when the status could not be determined.
    checkout_dirty: Option<bool>,
    /// Workloads executed per scenario, in run order.
    workloads: Vec<&'static str>,
    /// Parameters of the resize workload; `null` when no resize run was
    /// requested.
    resize: Option<ResizeParams>,
    /// Number of repetitions per scenario/workload pair (`--runs`).
    runs: usize,
}

/// Per-scenario outcome for one workload: the parameters actually rendered,
/// the observed synchronization shape, raw samples and their statistics.
/// With `--runs N > 1`, `statistics` is the mean of per-run statistics and
/// `runs` carries each run's own samples and stats.
#[derive(Debug, Clone, Serialize)]
struct ScenarioResult {
    name: &'static str,
    workload: &'static str,
    /// Name of the primary measured quantity in `samples_us`/`statistics`.
    metric: &'static str,
    /// What the primary metric covers for this workload.
    scope: &'static str,
    parameters: ScenarioParams,
    observed_synchronization: SyncObservations,
    /// Frame wall times in microseconds, converted from `Duration` without
    /// truncation to whole microseconds. Empty in smoke mode. With multiple
    /// runs this is the pooled concatenation of all runs' samples.
    samples_us: Vec<f64>,
    /// `null` in smoke mode; never fabricated from zero samples. With
    /// multiple runs this is the mean of per-run statistics.
    statistics: Option<FrameStats>,
    /// Per-stage breakdown for the resize workload; `null` for static.
    stages: Option<StageMetrics>,
    /// Per-run raw results; `null` for a single run or smoke mode.
    runs: Option<Vec<RunResult>>,
}

/// One repetition's raw outcome inside a multi-run `ScenarioResult`.
#[derive(Debug, Clone, Serialize)]
struct RunResult {
    samples_us: Vec<f64>,
    statistics: Option<FrameStats>,
    stages: Option<StageMetrics>,
}

/// Resize workload stage timings: element recomputation and frame rendering
/// measured separately inside the same frame.
#[derive(Debug, Clone, Serialize)]
struct StageMetrics {
    /// Wall time of `FocusRing::update_render_elements` + element collection
    /// + draw-parameter computation, per frame.
    update_us: Vec<f64>,
    /// Wall time of `draw_frame` (render + clear + finish + wait), per frame.
    render_us: Vec<f64>,
    /// `null` in smoke mode.
    update_statistics: Option<FrameStats>,
    /// `null` in smoke mode.
    render_statistics: Option<FrameStats>,
}

/// How many measured frames returned a `SyncPoint` that contained a fence.
/// A frame without a fence is still synchronized: in the checked Smithay
/// revision the no-fence path completes through `glFinish` inside
/// `finish_internal()`.
#[derive(Debug, Clone, Copy, Serialize)]
struct SyncObservations {
    frames_with_fence: usize,
    frames_without_fence: usize,
}

/// Serializable view of `ScenarioConfig`, built from the same factory output
/// that drives render element preparation. Colors are unpremultiplied
/// `[r, g, b, a]` floats in the 0–1 range.
#[derive(Debug, Clone, Serialize)]
struct ScenarioParams {
    focus_ring: FocusRingParams,
    knit: Option<KnitParams>,
}

#[derive(Debug, Clone, Serialize)]
struct FocusRingParams {
    off: bool,
    width: f64,
    active_color: [f32; 4],
    inactive_color: [f32; 4],
    urgent_color: [f32; 4],
    active_gradient: Option<GradientParams>,
    inactive_gradient: Option<GradientParams>,
    urgent_gradient: Option<GradientParams>,
}

#[derive(Debug, Clone, Serialize)]
struct GradientParams {
    from: [f32; 4],
    to: [f32; 4],
    angle: i16,
    relative_to: &'static str,
    color_space: &'static str,
    hue_interpolation: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct KnitParams {
    off: bool,
    pattern: &'static str,
    accent_color: [f32; 4],
    stitch_size: f64,
    relief: f64,
    fuzz: f64,
}

fn gradient_relative_to_name(relative_to: GradientRelativeTo) -> &'static str {
    match relative_to {
        GradientRelativeTo::Window => "window",
        GradientRelativeTo::WorkspaceView => "workspace-view",
    }
}

fn gradient_color_space_name(color_space: GradientColorSpace) -> &'static str {
    match color_space {
        GradientColorSpace::Srgb => "srgb",
        GradientColorSpace::SrgbLinear => "srgb-linear",
        GradientColorSpace::Oklab => "oklab",
        GradientColorSpace::Oklch => "oklch",
    }
}

fn hue_interpolation_name(hue_interpolation: HueInterpolation) -> &'static str {
    match hue_interpolation {
        HueInterpolation::Shorter => "shorter",
        HueInterpolation::Longer => "longer",
        HueInterpolation::Increasing => "increasing",
        HueInterpolation::Decreasing => "decreasing",
    }
}

fn knit_pattern_name(pattern: KnitPattern) -> &'static str {
    match pattern {
        KnitPattern::Stockinette => "stockinette",
        KnitPattern::Rib => "rib",
        KnitPattern::Checker => "checker",
        KnitPattern::Zigzag => "zigzag",
        KnitPattern::Diamond => "diamond",
        KnitPattern::Dots => "dots",
    }
}

fn gradient_params(gradient: &Gradient) -> GradientParams {
    GradientParams {
        from: gradient.from.to_array_unpremul(),
        to: gradient.to.to_array_unpremul(),
        angle: gradient.angle,
        relative_to: gradient_relative_to_name(gradient.relative_to),
        color_space: gradient_color_space_name(gradient.in_.color_space),
        hue_interpolation: hue_interpolation_name(gradient.in_.hue_interpolation),
    }
}

/// Converts the scenario configuration into its report form. Called on the
/// same `scenario_config()` output used for render element preparation, so
/// the report cannot drift from what was actually drawn.
fn scenario_params(config: &ScenarioConfig) -> ScenarioParams {
    let ring = &config.focus_ring;
    ScenarioParams {
        focus_ring: FocusRingParams {
            off: ring.off,
            width: ring.width,
            active_color: ring.active_color.to_array_unpremul(),
            inactive_color: ring.inactive_color.to_array_unpremul(),
            urgent_color: ring.urgent_color.to_array_unpremul(),
            active_gradient: ring.active_gradient.as_ref().map(gradient_params),
            inactive_gradient: ring.inactive_gradient.as_ref().map(gradient_params),
            urgent_gradient: ring.urgent_gradient.as_ref().map(gradient_params),
        },
        knit: config.knit.as_ref().map(|knit| KnitParams {
            off: knit.off,
            pattern: knit_pattern_name(knit.pattern),
            accent_color: knit.accent_color.to_array_unpremul(),
            stitch_size: knit.stitch_size,
            relief: knit.relief,
            fuzz: knit.fuzz,
        }),
    }
}

/// Reads a `GL_*` string through the renderer's context. Returns `None` when
/// the context or the string is unavailable; the pointer is checked before
/// `CStr` dereference and the value is copied into an owned `String`.
fn gl_string(renderer: &mut GlesRenderer, name: ffi::types::GLenum) -> Option<String> {
    renderer
        .with_context(|gl| unsafe {
            let ptr = gl.GetString(name);
            if ptr.is_null() {
                return None;
            }
            Some(
                CStr::from_ptr(ptr as *const _)
                    .to_string_lossy()
                    .into_owned(),
            )
        })
        .ok()
        .flatten()
}

/// Interprets the outcome of a captured git command: `Some` carries the
/// trimmed stdout of a successful invocation — including a successful empty
/// stdout as `Some("")` — while `None` means the command failed or could not
/// be run. Callers decide whether an empty result is meaningful.
fn interpret_git_output(success: bool, stdout: &[u8]) -> Option<String> {
    if !success {
        return None;
    }
    Some(String::from_utf8_lossy(stdout).trim().to_owned())
}

/// Runs a git command against the checkout containing this manifest and
/// returns trimmed stdout. Stdout and stderr are captured, so nothing leaks
/// into the program's own output. `None` when git fails or is unavailable;
/// a successful empty stdout is preserved as `Some("")`.
fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    interpret_git_output(output.status.success(), &output.stdout)
}

/// Interprets the `rev-parse HEAD` result: an empty HEAD (unborn branch or
/// otherwise unresolvable revision) is `None`, not a bogus revision string.
fn interpret_head_output(output: Option<String>) -> Option<String> {
    output.filter(|head| !head.is_empty())
}

/// Interprets the `status --porcelain` result: `Some(false)` for a clean
/// tree, `Some(true)` for a dirty one, `None` when the status could not be
/// determined.
fn interpret_status_output(output: Option<String>) -> Option<bool> {
    output.map(|status| !status.is_empty())
}

/// Best-effort runtime checkout state: HEAD and dirty flag of the working
/// tree at run time. This is not the revision the binary was built from.
/// `dirty` stays `None` (unknown) when the status cannot be determined.
fn runtime_checkout() -> (Option<String>, Option<bool>) {
    let head = interpret_head_output(git_output(&["rev-parse", "HEAD"]));
    // --porcelain=v1 pins the stable machine-readable format;
    // --untracked-files=normal overrides a user's status.showUntrackedFiles
    // setting so untracked files still count towards dirty.
    let dirty = interpret_status_output(git_output(&[
        "status",
        "--porcelain=v1",
        "--untracked-files=normal",
    ]));
    (head, dirty)
}

/// Collects run metadata once, before the measured series. GL strings come
/// from the live context; `GL_RENDERER` is the driver-reported renderer
/// string, not proof of a specific physical GPU.
fn collect_metadata(
    renderer: &mut GlesRenderer,
    scenarios: &[Scenario],
    workloads: &[Workload],
    warmup: usize,
    frames: usize,
    runs: usize,
    smoke: bool,
) -> Metadata {
    let (checkout_head, checkout_dirty) = runtime_checkout();
    Metadata {
        gl_vendor: gl_string(renderer, ffi::VENDOR),
        gl_renderer: gl_string(renderer, ffi::RENDERER),
        gl_version: gl_string(renderer, ffi::VERSION),
        package_version: env!("CARGO_PKG_VERSION"),
        build_version: niri::utils::version(),
        debug_assertions: cfg!(debug_assertions),
        target_size: [TARGET_SIZE.0, TARGET_SIZE.1],
        target_format: TARGET_FORMAT.to_string(),
        window_count: RING_COUNT,
        window_content_size: [WIN_SIZE.0, WIN_SIZE.1],
        border_width: BORDER_WIDTH,
        outer_corner_radius: OUTER_RADIUS,
        scale: SCALE,
        alpha: ALPHA,
        element_draws_per_frame: RING_COUNT * ELEMENTS_PER_RING,
        mode: if smoke { "smoke" } else { "benchmark" },
        warmup_frames: if smoke { 0 } else { warmup },
        measured_frames: if smoke { 0 } else { frames },
        scenarios: scenarios.iter().map(|s| s.name()).collect(),
        revision_source: "runtime_checkout",
        checkout_head,
        checkout_dirty,
        workloads: workloads.iter().map(|w| w.name()).collect(),
        resize: workloads
            .contains(&Workload::Resize)
            .then_some(ResizeParams {
                min_window_size: [RESIZE_MIN_SIZE.0, RESIZE_MIN_SIZE.1],
                max_window_size: [WIN_SIZE.0, WIN_SIZE.1],
                cycle_steps: RESIZE_CYCLE_STEPS,
                easing: "cosine",
            }),
        runs,
    }
}

/// A render element with everything `draw` needs precomputed during
/// preparation: source region, destination and full-rect damage. `damage`
/// covers the whole element; preparation rejects any element that does not
/// fit the target, so a draw can never be skipped silently.
struct PreparedElement {
    element: FocusRingRenderElement,
    src: Rectangle<f64, Buffer>,
    dst: Rectangle<i32, Physical>,
    damage: Rectangle<i32, Physical>,
    cache: UserDataMap,
}

/// Everything needed to draw frames of a scenario: the `FocusRing`s that
/// produce the elements, the prepared elements for the current frame and
/// the texture target bound once for the whole frame series.
struct PreparedScenario<'a> {
    rings: Vec<FocusRing>,
    elements: Vec<PreparedElement>,
    target: GlesTarget<'a>,
}

/// Checks that an element's destination rectangle is non-empty and fully
/// contained in the render target. Touching the target edges is allowed;
/// any partial or complete exit is rejected. `contains_rect` compares
/// saturating sums, so the check cannot overflow.
fn dst_fits_in_target(dst: Rectangle<i32, Physical>, target: Rectangle<i32, Physical>) -> bool {
    dst.size.w > 0 && dst.size.h > 0 && target.contains_rect(dst)
}

/// Runs scenario preparation once: checks the shader path, creates the
/// `FocusRing`s, builds the initial frame's elements and binds the shared
/// texture target.
fn prepare_scenario<'a>(
    renderer: &mut GlesRenderer,
    texture: &'a mut GlesTexture,
    scenario: Scenario,
    workload: Workload,
) -> anyhow::Result<PreparedScenario<'a>> {
    ensure!(
        BorderRenderElement::has_shader(renderer),
        "border shader is not available for scenario {}",
        scenario.name()
    );

    let rings = create_rings(scenario);
    let target = renderer.bind(texture).context("error binding texture")?;

    let mut prepared = PreparedScenario {
        rings,
        elements: Vec::new(),
        target,
    };

    // The initial frame state: full size for static, cycle minimum for
    // resize. Both workloads then reuse `rebuild_elements` per frame.
    let win_size = match workload {
        Workload::Static => Size::from(WIN_SIZE),
        Workload::Resize => resize_win_size(0),
    };
    rebuild_elements(renderer, &mut prepared, scenario, win_size)
        .context("error preparing initial frame")?;

    Ok(prepared)
}

/// Creates the six `FocusRing`s for a scenario. Rings persist across frames
/// so `update_render_elements` reuses their internal storage instead of
/// reallocating it every frame.
fn create_rings(scenario: Scenario) -> Vec<FocusRing> {
    let config = scenario_config(scenario);
    (0..RING_COUNT)
        .map(|_| {
            let mut ring = FocusRing::new(config.focus_ring);
            ring.update_knit(config.knit);
            ring
        })
        .collect()
}

/// Recomputes ring geometry for `win_size`, collects the render elements and
/// precomputes their draw parameters. This is the per-frame update stage of
/// the resize workload; for static it runs once during preparation.
fn rebuild_elements(
    renderer: &mut GlesRenderer,
    prepared: &mut PreparedScenario,
    scenario: Scenario,
    win_size: Size<f64, Logical>,
) -> anyhow::Result<()> {
    // Only used for workspace-relative gradients, which are disabled here.
    let view_rect = Rectangle::new(
        Point::from((-BORDER_WIDTH, -BORDER_WIDTH)),
        win_size + Size::from((BORDER_WIDTH * 2., BORDER_WIDTH * 2.)),
    );

    let scale = Scale::from(SCALE);
    let output_rect = Rectangle::from_size(Size::<i32, Physical>::from(TARGET_SIZE));

    prepared.elements.clear();
    for (i, ring) in prepared.rings.iter_mut().enumerate() {
        ring.update_render_elements(
            win_size,
            true,
            true,
            false,
            view_rect,
            CornerRadius::from(OUTER_RADIUS),
            SCALE,
            ALPHA,
        );

        // `location` is the window contents origin; the ring extends
        // BORDER_WIDTH outward on every side.
        let location = Point::from((BORDER_WIDTH + i as f64 * RING_STEP, BORDER_WIDTH));
        ring.render(renderer, location, &mut |element| {
            let src = element.src();
            let dst = element.geometry(scale);
            // Damage is relative to the element's dst origin; cover it fully
            // so damage tracking cannot skip the scene.
            let damage = Rectangle::new(Point::default(), dst.size);
            prepared.elements.push(PreparedElement {
                element,
                src,
                dst,
                damage,
                cache: UserDataMap::new(),
            });
        });
    }

    // The workload is fixed at 48 draws per frame: an element that does not
    // fit the target is a preparation error, not a skippable draw.
    for (i, elem) in prepared.elements.iter().enumerate() {
        ensure!(
            dst_fits_in_target(elem.dst, output_rect),
            "scenario {}: element {} dst {:?} does not fit target {:?}",
            scenario.name(),
            i,
            elem.dst,
            output_rect
        );
    }

    ensure!(
        prepared.elements.len() == RING_COUNT * ELEMENTS_PER_RING,
        "scenario {}: expected {} render elements, got {}",
        scenario.name(),
        RING_COUNT * ELEMENTS_PER_RING,
        prepared.elements.len()
    );
    ensure!(
        prepared
            .elements
            .iter()
            .all(|elem| matches!(elem.element, FocusRingRenderElement::Gradient(_))),
        "scenario {}: some elements fell back to SolidColorRenderElement",
        scenario.name()
    );

    Ok(())
}

/// Draws one complete frame: clear, all prepared elements with full damage,
/// finish and wait for confirmed completion. Returns whether the `SyncPoint`
/// returned by `finish()` contained a fence.
///
/// Frame completion is guaranteed by `GlesFrame::finish_internal()` in
/// Smithay: it returns an `EGLFence`-backed `SyncPoint` when the
/// `ExportFence` capability is present (`SyncPoint::wait()` then blocks in
/// `eglClientWaitSync`), and falls back to a synchronous `glFinish()` plus an
/// already-signaled `SyncPoint` otherwise. No extra `glFinish` is needed.
fn draw_frame(
    renderer: &mut GlesRenderer,
    prepared: &mut PreparedScenario,
) -> anyhow::Result<bool> {
    let size = Size::<i32, Physical>::from(TARGET_SIZE);
    let output_rect = Rectangle::from_size(size);

    let mut frame = renderer
        .render(&mut prepared.target, size, Transform::Normal)
        .context("error starting frame")?;
    frame
        .clear(
            smithay::backend::renderer::Color32F::TRANSPARENT,
            &[output_rect],
        )
        .context("error clearing")?;

    for elem in &prepared.elements {
        if elem.element.is_framebuffer_effect() {
            RenderElement::<GlesRenderer>::capture_framebuffer(
                &elem.element,
                &mut frame,
                elem.src,
                elem.dst,
                &elem.cache,
            )
            .context("error in capture_framebuffer()")?;
        }
        RenderElement::<GlesRenderer>::draw(
            &elem.element,
            &mut frame,
            elem.src,
            elem.dst,
            &[elem.damage],
            &[],
            Some(&elem.cache),
        )
        .context("error drawing element")?;
    }

    let sync = frame.finish().context("error finishing frame")?;
    sync.wait().context("error waiting for rendering")?;
    Ok(sync.contains_fence())
}

/// Drains GL commands submitted during setup (element/shader preparation,
/// texture bind) so they cannot leak into the first measured frame. An empty
/// `render()` + `finish()` + `wait()` sequence is a pure synchronization
/// barrier: it draws nothing and is not a warmup frame.
fn finish_pending_work(
    renderer: &mut GlesRenderer,
    prepared: &mut PreparedScenario,
) -> anyhow::Result<()> {
    let size = Size::<i32, Physical>::from(TARGET_SIZE);
    let frame = renderer
        .render(&mut prepared.target, size, Transform::Normal)
        .context("error starting barrier frame")?;
    let sync = frame.finish().context("error finishing barrier frame")?;
    sync.wait().context("error waiting for barrier frame")?;
    Ok(())
}

/// Reads the finished frame back and writes it as `<name>.png`. Only runs
/// when `--dump-dir` is given, after the whole frame series is complete. The
/// "saved" message goes to stderr in JSON mode so stdout stays a single
/// JSON document.
fn save_png(
    renderer: &mut GlesRenderer,
    target: &GlesTarget,
    name: &str,
    dir: &Path,
    json: bool,
) -> anyhow::Result<()> {
    let mapping =
        copy_framebuffer(renderer, target, TARGET_FORMAT).context("error copying framebuffer")?;
    let pixels = renderer
        .map_texture(&mapping)
        .context("error mapping texture")?;

    let path = dir.join(format!("{name}.png"));
    let file = std::fs::File::create(&path)
        .with_context(|| format!("error creating {}", path.display()))?;
    write_png_rgba8(
        std::io::BufWriter::new(file),
        TARGET_SIZE.0 as u32,
        TARGET_SIZE.1 as u32,
        pixels,
    )
    .with_context(|| format!("error encoding {}", path.display()))?;
    if json {
        eprintln!("saved {}", path.display());
    } else {
        println!("saved {}", path.display());
    }
    Ok(())
}

/// Statistics over measured `frame_wall_time_us` samples, in microseconds.
#[derive(Debug, Clone, Copy, Serialize)]
struct FrameStats {
    count: usize,
    min_us: f64,
    median_us: f64,
    mean_us: f64,
    p95_us: f64,
    max_us: f64,
}

/// Computes statistics over frame wall times. Median averages the two central
/// values for even counts; p95 uses the nearest-rank method
/// (`ceil(0.95 * N) - 1` in the sorted array). Empty input is an error.
fn frame_stats(samples: &[Duration]) -> anyhow::Result<FrameStats> {
    ensure!(!samples.is_empty(), "no samples to compute statistics over");

    let mut us: Vec<f64> = samples
        .iter()
        .map(|d| d.as_nanos() as f64 / 1000.)
        .collect();
    us.sort_by(f64::total_cmp);

    let n = us.len();
    let min_us = us[0];
    let max_us = us[n - 1];
    let mean_us = us.iter().sum::<f64>() / n as f64;
    let median_us = if n % 2 == 1 {
        us[n / 2]
    } else {
        (us[n / 2 - 1] + us[n / 2]) / 2.
    };
    let p95_us = us[(0.95 * n as f64).ceil() as usize - 1];

    Ok(FrameStats {
        count: n,
        min_us,
        median_us,
        mean_us,
        p95_us,
        max_us,
    })
}

/// Mean of per-run statistics: every field except `count` is averaged across
/// runs; `count` is the total number of measured frames. Empty input is an
/// error. This matches the aggregation used for multi-run comparisons.
fn mean_frame_stats(stats: &[FrameStats]) -> anyhow::Result<FrameStats> {
    ensure!(!stats.is_empty(), "no run statistics to average");
    let n = stats.len() as f64;
    Ok(FrameStats {
        count: stats.iter().map(|s| s.count).sum(),
        min_us: stats.iter().map(|s| s.min_us).sum::<f64>() / n,
        median_us: stats.iter().map(|s| s.median_us).sum::<f64>() / n,
        mean_us: stats.iter().map(|s| s.mean_us).sum::<f64>() / n,
        p95_us: stats.iter().map(|s| s.p95_us).sum::<f64>() / n,
        max_us: stats.iter().map(|s| s.max_us).sum::<f64>() / n,
    })
}

/// Aggregates `runs` repetitions of one scenario/workload pair into the
/// reported `ScenarioResult`: pooled samples, mean-of-run statistics, summed
/// fence observations and per-run raw data.
fn aggregate_runs(runs: Vec<ScenarioResult>) -> anyhow::Result<ScenarioResult> {
    let first = runs
        .first()
        .context("aggregate_runs requires at least one run")?;

    let run_stats: Vec<FrameStats> = runs.iter().filter_map(|r| r.statistics).collect();
    let statistics = if run_stats.is_empty() {
        None
    } else {
        Some(mean_frame_stats(&run_stats)?)
    };

    let stages = first.stages.as_ref().map(|_| {
        let upd: Vec<FrameStats> = runs
            .iter()
            .filter_map(|r| r.stages.as_ref()?.update_statistics)
            .collect();
        let ren: Vec<FrameStats> = runs
            .iter()
            .filter_map(|r| r.stages.as_ref()?.render_statistics)
            .collect();
        StageMetrics {
            update_us: runs
                .iter()
                .flat_map(|r| {
                    r.stages
                        .as_ref()
                        .map(|s| s.update_us.clone())
                        .unwrap_or_default()
                })
                .collect(),
            render_us: runs
                .iter()
                .flat_map(|r| {
                    r.stages
                        .as_ref()
                        .map(|s| s.render_us.clone())
                        .unwrap_or_default()
                })
                .collect(),
            update_statistics: if upd.is_empty() {
                None
            } else {
                mean_frame_stats(&upd).ok()
            },
            render_statistics: if ren.is_empty() {
                None
            } else {
                mean_frame_stats(&ren).ok()
            },
        }
    });

    let per_run: Vec<RunResult> = runs
        .iter()
        .map(|r| RunResult {
            samples_us: r.samples_us.clone(),
            statistics: r.statistics,
            stages: r.stages.clone(),
        })
        .collect();

    Ok(ScenarioResult {
        name: first.name,
        workload: first.workload,
        metric: first.metric,
        scope: first.scope,
        parameters: first.parameters.clone(),
        observed_synchronization: SyncObservations {
            frames_with_fence: runs
                .iter()
                .map(|r| r.observed_synchronization.frames_with_fence)
                .sum(),
            frames_without_fence: runs
                .iter()
                .map(|r| r.observed_synchronization.frames_without_fence)
                .sum(),
        },
        samples_us: runs.iter().flat_map(|r| r.samples_us.clone()).collect(),
        statistics,
        stages,
        runs: (runs.len() > 1).then_some(per_run),
    })
}

/// Runs one scenario under one workload: prepare once, drain setup work,
/// optional warmup frames, measured frames, statistics, optional PNG dump.
/// Returns the report entry for the scenario/workload pair.
fn run_scenario(
    renderer: &mut GlesRenderer,
    texture: &mut GlesTexture,
    scenario: Scenario,
    workload: Workload,
    warmup: usize,
    frames: usize,
    smoke: bool,
    dump_dir: Option<&Path>,
    json: bool,
) -> anyhow::Result<ScenarioResult> {
    let mut prepared =
        prepare_scenario(renderer, texture, scenario, workload).with_context(|| {
            format!(
                "scenario {} [{}]: error preparing",
                scenario.name(),
                workload.name()
            )
        })?;

    // Synchronize pending setup commands before the series; not timed.
    finish_pending_work(renderer, &mut prepared).with_context(|| {
        format!(
            "scenario {} [{}]: error in setup barrier",
            scenario.name(),
            workload.name()
        )
    })?;

    let mut samples: Vec<Duration> = Vec::new();
    let mut update_samples: Vec<Duration> = Vec::new();
    let mut render_samples: Vec<Duration> = Vec::new();
    let mut frames_with_fence = 0usize;
    let mut frames_without_fence = 0usize;
    // Frames whose fence observation is counted: measured frames plus smoke
    // frames. Warmup fences are ignored, matching the static path.
    let mut counted_frames = 0usize;

    match (workload, smoke) {
        (Workload::Static, true) => {
            // One real frame per scenario; its fence observation is counted
            // but no samples or statistics are produced.
            let had_fence = draw_frame(renderer, &mut prepared).with_context(|| {
                format!(
                    "scenario {} [static]: error in smoke frame",
                    scenario.name()
                )
            })?;
            counted_frames += 1;
            if had_fence {
                frames_with_fence += 1;
            } else {
                frames_without_fence += 1;
            }
        }
        (Workload::Static, false) => {
            samples.reserve(frames);
            for _ in 0..warmup {
                draw_frame(renderer, &mut prepared).with_context(|| {
                    format!(
                        "scenario {} [static]: error in warmup frame",
                        scenario.name()
                    )
                })?;
            }

            for _ in 0..frames {
                let start = Instant::now();
                let had_fence = draw_frame(renderer, &mut prepared).with_context(|| {
                    format!(
                        "scenario {} [static]: error in measured frame",
                        scenario.name()
                    )
                })?;
                // Timing ends here; bookkeeping happens after elapsed is fixed.
                samples.push(start.elapsed());
                counted_frames += 1;
                if had_fence {
                    frames_with_fence += 1;
                } else {
                    frames_without_fence += 1;
                }
            }
        }
        (Workload::Resize, smoke_mode) => {
            // One resize step per frame. Smoke runs exactly one full cycle
            // unmeasured so every intermediate size is validated.
            let smoke_steps = if smoke_mode { RESIZE_CYCLE_STEPS } else { 0 };
            let warmup_steps = if smoke_mode { 0 } else { warmup };
            let measured = if smoke_mode { 0 } else { frames };
            let total_steps = warmup_steps + measured + smoke_steps;

            samples.reserve(measured);
            update_samples.reserve(measured);
            render_samples.reserve(measured);

            for i in 0..total_steps {
                // Step 0 is the prepared minimum; measured steps are 1..=60
                // so the cycle ends exactly back at the minimum.
                let step = i % RESIZE_CYCLE_STEPS + 1;
                let is_measured = !smoke_mode && i >= warmup_steps;

                let frame_start = Instant::now();
                let update_start = Instant::now();
                rebuild_elements(renderer, &mut prepared, scenario, resize_win_size(step))
                    .with_context(|| {
                        format!(
                            "scenario {} [resize]: error in resize update",
                            scenario.name()
                        )
                    })?;
                let update_elapsed = update_start.elapsed();
                let had_fence = draw_frame(renderer, &mut prepared).with_context(|| {
                    format!("scenario {} [resize]: error in frame", scenario.name())
                })?;
                let total_elapsed = frame_start.elapsed();

                if is_measured || smoke_mode {
                    counted_frames += 1;
                    if had_fence {
                        frames_with_fence += 1;
                    } else {
                        frames_without_fence += 1;
                    }
                }
                if is_measured {
                    update_samples.push(update_elapsed);
                    render_samples.push(total_elapsed - update_elapsed);
                    samples.push(total_elapsed);
                }
            }
        }
    }

    ensure!(
        frames_with_fence + frames_without_fence == counted_frames,
        "scenario {} [{}]: sync observations do not match counted frames",
        scenario.name(),
        workload.name()
    );

    if let Some(dir) = dump_dir {
        match workload {
            Workload::Static => {
                save_png(renderer, &prepared.target, scenario.name(), dir, json).with_context(
                    || format!("scenario {} [static]: error saving png", scenario.name()),
                )?;
            }
            Workload::Resize => {
                // Re-render the two cycle extremes for visual inspection.
                for (step, label) in [(0, "min"), (RESIZE_CYCLE_STEPS / 2, "max")] {
                    rebuild_elements(renderer, &mut prepared, scenario, resize_win_size(step))
                        .with_context(|| {
                            format!(
                                "scenario {} [resize]: error rebuilding {label} frame",
                                scenario.name()
                            )
                        })?;
                    draw_frame(renderer, &mut prepared).with_context(|| {
                        format!(
                            "scenario {} [resize]: error drawing {label} frame",
                            scenario.name()
                        )
                    })?;
                    let name = format!("{}-resize-{label}", scenario.name());
                    save_png(renderer, &prepared.target, &name, dir, json).with_context(|| {
                        format!("scenario {} [resize]: error saving png", scenario.name())
                    })?;
                }
            }
        }
    }

    let statistics = if samples.is_empty() {
        None
    } else {
        Some(frame_stats(&samples).with_context(|| {
            format!(
                "scenario {} [{}]: error computing stats",
                scenario.name(),
                workload.name()
            )
        })?)
    };

    let stages = match workload {
        Workload::Static => None,
        Workload::Resize => Some(StageMetrics {
            update_us: update_samples
                .iter()
                .map(|d| d.as_nanos() as f64 / 1000.)
                .collect(),
            render_us: render_samples
                .iter()
                .map(|d| d.as_nanos() as f64 / 1000.)
                .collect(),
            update_statistics: if update_samples.is_empty() {
                None
            } else {
                Some(frame_stats(&update_samples).with_context(|| {
                    format!(
                        "scenario {} [resize]: error computing update stats",
                        scenario.name()
                    )
                })?)
            },
            render_statistics: if render_samples.is_empty() {
                None
            } else {
                Some(frame_stats(&render_samples).with_context(|| {
                    format!(
                        "scenario {} [resize]: error computing render stats",
                        scenario.name()
                    )
                })?)
            },
        }),
    };

    Ok(ScenarioResult {
        name: scenario.name(),
        workload: workload.name(),
        metric: "frame_wall_time_us",
        scope: match workload {
            Workload::Static => "renderer.render() + clear + element_draws_per_frame RenderElement::draw calls + finish + completion wait",
            Workload::Resize => "FocusRing::update_render_elements + element collection + renderer.render() + clear + element_draws_per_frame RenderElement::draw calls + finish + completion wait",
        },
        parameters: scenario_params(&scenario_config(scenario)),
        observed_synchronization: SyncObservations {
            frames_with_fence,
            frames_without_fence,
        },
        samples_us: samples
            .iter()
            .map(|d| d.as_nanos() as f64 / 1000.)
            .collect(),
        statistics,
        stages,
        runs: None,
    })
}

fn run(
    renderer: &mut GlesRenderer,
    scenarios: &[Scenario],
    workloads: &[Workload],
    warmup: usize,
    frames: usize,
    runs: usize,
    smoke: bool,
    dump_dir: Option<&Path>,
    json: bool,
) -> anyhow::Result<()> {
    if cfg!(debug_assertions) {
        eprintln!("warning: debug build, results must not be used for performance evaluation");
    }

    if let Some(dir) = dump_dir {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("error creating dump dir {}", dir.display()))?;
    }

    let size = Size::<i32, Physical>::from(TARGET_SIZE);
    let mut texture =
        create_texture(renderer, size, TARGET_FORMAT).context("error creating texture")?;

    // Metadata is collected once, outside the measured series.
    let metadata = collect_metadata(renderer, scenarios, workloads, warmup, frames, runs, smoke);

    if !json {
        println!(
            "GL_VENDOR = {}",
            metadata.gl_vendor.as_deref().unwrap_or("unknown")
        );
        println!(
            "GL_RENDERER = {}",
            metadata.gl_renderer.as_deref().unwrap_or("unknown")
        );
        println!(
            "GL_VERSION = {}",
            metadata.gl_version.as_deref().unwrap_or("unknown")
        );
        println!("build = {}", metadata.build_version);
        println!(
            "checkout = {} ({}) [{}]",
            metadata.checkout_head.as_deref().unwrap_or("unknown"),
            match metadata.checkout_dirty {
                Some(true) => "dirty",
                Some(false) => "clean",
                None => "dirty status unknown",
            },
            metadata.revision_source
        );
        println!("debug_assertions = {}", metadata.debug_assertions);
        println!(
            "target = {}x{} {}",
            metadata.target_size[0], metadata.target_size[1], metadata.target_format
        );
        println!(
            "windows = {} x {}x{}, border_width = {}, outer_radius = {}, scale = {}, alpha = {}",
            metadata.window_count,
            metadata.window_content_size[0],
            metadata.window_content_size[1],
            metadata.border_width,
            metadata.outer_corner_radius,
            metadata.scale,
            metadata.alpha
        );
        println!("mode = {}", metadata.mode);
        println!("workloads = {}", metadata.workloads.join(", "));
        if runs > 1 {
            println!("runs = {runs} (statistics are means of per-run statistics)");
        }
        if !smoke {
            println!("metric = frame_wall_time_us");
            println!(
                "scope = clear + {} element draws + finish + completion wait",
                metadata.element_draws_per_frame
            );
            println!(
                "scenario | workload | frames | min_us | median_us | mean_us | p95_us | max_us"
            );
        }
    }

    // Outer loop over repetitions: each run re-prepares every scenario so
    // allocator and driver state see the same cold path each time.
    let mut all_runs: Vec<Vec<ScenarioResult>> = Vec::with_capacity(runs);
    for i in 0..runs {
        if runs > 1 {
            eprintln!("run {}/{}...", i + 1, runs);
        }
        let mut run_results = Vec::with_capacity(scenarios.len() * workloads.len());
        for &scenario in scenarios {
            for &workload in workloads {
                run_results.push(run_scenario(
                    renderer,
                    &mut texture,
                    scenario,
                    workload,
                    warmup,
                    frames,
                    smoke,
                    dump_dir,
                    json,
                )?);
            }
        }
        all_runs.push(run_results);
    }

    // Aggregate per scenario/workload pair across runs.
    let mut results = Vec::with_capacity(scenarios.len() * workloads.len());
    for pair_idx in 0..scenarios.len() * workloads.len() {
        let pair_runs: Vec<ScenarioResult> = all_runs.iter().map(|r| r[pair_idx].clone()).collect();
        results.push(aggregate_runs(pair_runs)?);
    }

    if !json {
        for result in &results {
            if let Some(stats) = &result.statistics {
                println!(
                    "{} | {} | {} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2}",
                    result.name,
                    result.workload,
                    stats.count,
                    stats.min_us,
                    stats.median_us,
                    stats.mean_us,
                    stats.p95_us,
                    stats.max_us,
                );
                if let Some(stages) = &result.stages {
                    if let (Some(upd), Some(ren)) =
                        (&stages.update_statistics, &stages.render_statistics)
                    {
                        println!(
                            "  update: median {:.2} mean {:.2} | render: median {:.2} mean {:.2}",
                            upd.median_us, upd.mean_us, ren.median_us, ren.mean_us,
                        );
                    }
                }
            }
        }

        // Total / Summary across all scenario/workload pairs, matching the
        // multi-run comparison format.
        if results.len() > 1 {
            let measured: Vec<&ScenarioResult> =
                results.iter().filter(|r| r.statistics.is_some()).collect();
            if !measured.is_empty() {
                let n = measured.len() as f64;
                let avg = |f: fn(&FrameStats) -> f64| {
                    measured
                        .iter()
                        .map(|r| f(r.statistics.as_ref().unwrap()))
                        .sum::<f64>()
                        / n
                };
                // Total render time: mean over runs of the sum of
                // mean_us * count across all pairs in that run.
                let render_ms: f64 = all_runs
                    .iter()
                    .map(|run| {
                        run.iter()
                            .filter_map(|r| r.statistics.map(|s| s.mean_us * s.count as f64))
                            .sum::<f64>()
                            / 1000.
                    })
                    .sum::<f64>()
                    / all_runs.len() as f64;
                println!(
                    "Total / Summary ({} pairs, {} runs, {} frames each):",
                    measured.len(),
                    runs,
                    frames
                );
                println!("  min:    {:.1} us", avg(|s| s.min_us));
                println!("  med:    {:.1} us", avg(|s| s.median_us));
                println!("  p95:    {:.1} us", avg(|s| s.p95_us));
                println!("  render: {:.1} ms", render_ms);
            }
        }
    }

    if json {
        let report = Report {
            schema_version: 3,
            methodology_version: 3,
            metadata,
            results,
        };
        // Serialize fully before writing: a serialization error must not
        // leave a partial document on stdout.
        let out = serde_json::to_string(&report).context("error serializing report")?;
        println!("{out}");
    }

    Ok(())
}

fn main() -> anyhow::Result<()> {
    let mut scenarios = Vec::new();
    let mut workloads = Vec::new();
    let mut dump_dir = None;
    let mut warmup = 30usize;
    let mut frames = 300usize;
    let mut runs = 1usize;
    let mut smoke = false;
    let mut json = false;
    let mut warmup_set = false;
    let mut frames_set = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--scenario" => {
                let name = args.next().context("--scenario requires a name")?;
                if name == "all" {
                    scenarios = Scenario::ALL.to_vec();
                } else {
                    let scenario = Scenario::from_name(&name).with_context(|| {
                        format!(
                            "unknown scenario: {name} (expected one of: all, {})",
                            Scenario::ALL.map(Scenario::name).join(", ")
                        )
                    })?;
                    scenarios.push(scenario);
                }
            }
            "--workload" => {
                let name = args.next().context("--workload requires a name")?;
                if name == "all" {
                    workloads = Workload::ALL.to_vec();
                } else {
                    let workload = Workload::from_name(&name).with_context(|| {
                        format!(
                            "unknown workload: {name} (expected one of: all, {})",
                            Workload::ALL.map(Workload::name).join(", ")
                        )
                    })?;
                    workloads.push(workload);
                }
            }
            "--dump-dir" => {
                dump_dir = Some(PathBuf::from("target/border_bench"));
            }
            "--warmup" => {
                let value = args.next().context("--warmup requires a number")?;
                warmup = value
                    .parse()
                    .with_context(|| format!("invalid --warmup value: {value}"))?;
                warmup_set = true;
            }
            "--frames" => {
                let value = args.next().context("--frames requires a number")?;
                frames = value
                    .parse()
                    .with_context(|| format!("invalid --frames value: {value}"))?;
                frames_set = true;
            }
            "--runs" => {
                let value = args.next().context("--runs requires a number")?;
                runs = value
                    .parse()
                    .with_context(|| format!("invalid --runs value: {value}"))?;
            }
            "--smoke" => smoke = true,
            "--json" => json = true,
            // Cargo passes `--bench` to the benchmark executable when it is
            // run through `cargo bench`; it is a harness marker, not a flag.
            "--bench" => {}
            other => bail!("unknown argument: {other}"),
        }
    }
    if scenarios.is_empty() {
        scenarios = Scenario::ALL.to_vec();
    }
    if workloads.is_empty() {
        workloads = vec![Workload::Static];
    }

    ensure!(
        !(smoke && (warmup_set || frames_set)),
        "--smoke cannot be combined with --warmup or --frames"
    );
    ensure!(frames > 0, "--frames must be greater than 0");
    ensure!(runs > 0, "--runs must be greater than 0");
    ensure!(
        smoke || !workloads.contains(&Workload::Resize) || frames % RESIZE_CYCLE_STEPS == 0,
        "--frames must be a multiple of {RESIZE_CYCLE_STEPS} (the resize cycle length) \
         for the resize workload"
    );

    let mut headless = Headless::new();
    // Initializes resources and shaders internally; do not init them again.
    headless
        .add_renderer()
        .context("error creating headless renderer")?;

    headless
        .with_primary_renderer(|renderer| {
            run(
                renderer,
                &scenarios,
                &workloads,
                warmup,
                frames,
                runs,
                smoke,
                dump_dir.as_deref(),
                json,
            )
        })
        .context("no primary renderer")?
        .context("error running benchmark")?;

    Ok(())
}

// Cargo compiles bench targets with `--cfg test` even with `harness = false`,
// so this module is built into the benchmark binary where `#[test]` functions
// are not libtest roots and the helpers below would warn as unused.
#[cfg(test)]
#[allow(dead_code)]
mod tests {
    use super::*;

    fn config(scenario: Scenario) -> ScenarioConfig {
        scenario_config(scenario)
    }

    fn us(micros: f64) -> Duration {
        Duration::from_nanos((micros * 1000.).round() as u64)
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-6,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn all_scenarios_share_common_frame_config() {
        for scenario in Scenario::ALL {
            let config = config(scenario).focus_ring;
            assert!(
                !config.off,
                "{}: focus ring must be enabled",
                scenario.name()
            );
            assert_eq!(
                config.width,
                BORDER_WIDTH,
                "{}: unexpected border width",
                scenario.name()
            );
            assert_eq!(
                config.active_color,
                base_color(),
                "{}: unexpected active color",
                scenario.name()
            );
            assert_eq!(
                config.inactive_color,
                base_color(),
                "{}: unexpected inactive color",
                scenario.name()
            );
            assert!(
                config.inactive_gradient.is_none(),
                "{}: inactive gradient must be unset",
                scenario.name()
            );
            assert!(
                config.urgent_gradient.is_none(),
                "{}: urgent gradient must be unset",
                scenario.name()
            );
        }
    }

    #[test]
    fn solid_has_no_gradient_and_no_knit() {
        let config = config(Scenario::Solid);
        assert!(config.focus_ring.active_gradient.is_none());
        assert!(config.knit.is_none());
    }

    #[test]
    fn gradient_srgb_uses_srgb_window_gradient() {
        let config = config(Scenario::GradientSrgb);
        let gradient = config
            .focus_ring
            .active_gradient
            .expect("gradient-srgb must set active_gradient");
        assert_eq!(gradient.from, base_color());
        assert_eq!(gradient.to, gradient_to());
        assert_eq!(gradient.angle, 90);
        assert_eq!(gradient.relative_to, GradientRelativeTo::Window);
        assert_eq!(gradient.in_.color_space, GradientColorSpace::Srgb);
        assert!(config.knit.is_none());
    }

    #[test]
    fn gradient_oklch_uses_oklch_longer_hue() {
        let config = config(Scenario::GradientOklch);
        let gradient = config
            .focus_ring
            .active_gradient
            .expect("gradient-oklch must set active_gradient");
        assert_eq!(gradient.from, base_color());
        assert_eq!(gradient.to, gradient_to());
        assert_eq!(gradient.angle, 90);
        assert_eq!(gradient.relative_to, GradientRelativeTo::Window);
        assert_eq!(gradient.in_.color_space, GradientColorSpace::Oklch);
        assert_eq!(gradient.in_.hue_interpolation, HueInterpolation::Longer);
        assert!(config.knit.is_none());
    }

    #[test]
    fn knit_scenarios_enable_knit_without_gradient() {
        for scenario in [
            Scenario::KnitStockinette,
            Scenario::KnitZigzag,
            Scenario::KnitZigzagFuzz,
            Scenario::KnitZigzagDetail,
        ] {
            let config = config(scenario);
            let knit = config
                .knit
                .unwrap_or_else(|| panic!("{}: knit must be enabled", scenario.name()));
            assert!(!knit.off, "{}: knit must not be off", scenario.name());
            assert_eq!(
                knit.accent_color,
                knit_accent(),
                "{}: unexpected knit accent color",
                scenario.name()
            );
            assert_eq!(
                config.focus_ring.active_color,
                base_color(),
                "{}: unexpected knit base color",
                scenario.name()
            );
            assert!(
                config.focus_ring.active_gradient.is_none(),
                "{}: knit scenarios must not set a gradient",
                scenario.name()
            );
        }
    }

    #[test]
    fn knit_gradient_scenarios_enable_knit_with_gradient() {
        for scenario in [
            Scenario::KnitGradientStockinette,
            Scenario::KnitGradientRib,
            Scenario::KnitGradientChecker,
            Scenario::KnitGradientZigzag,
            Scenario::KnitGradientDiamond,
            Scenario::KnitGradientDots,
        ] {
            let config = config(scenario);
            let knit = config
                .knit
                .unwrap_or_else(|| panic!("{}: knit must be enabled", scenario.name()));
            assert!(!knit.off, "{}: knit must not be off", scenario.name());
            assert!(
                config.focus_ring.active_gradient.is_some(),
                "{}: knit gradient scenarios must set a gradient",
                scenario.name()
            );
        }
    }

    #[test]
    fn stockinette_and_zigzag_differ_only_in_pattern() {
        let a = config(Scenario::KnitStockinette);
        let b = config(Scenario::KnitZigzag);
        assert_eq!(a.focus_ring, b.focus_ring);
        let a = a.knit.unwrap();
        let b = b.knit.unwrap();
        assert_ne!(a.pattern, b.pattern);
        assert_eq!(a.pattern, KnitPattern::Stockinette);
        assert_eq!(b.pattern, KnitPattern::Zigzag);
        assert_eq!(
            KnitBorder {
                pattern: a.pattern,
                ..b
            },
            a
        );
    }

    #[test]
    fn zigzag_and_zigzag_fuzz_differ_only_in_fuzz() {
        let a = config(Scenario::KnitZigzag).knit.unwrap();
        let b = config(Scenario::KnitZigzagFuzz).knit.unwrap();
        assert_ne!(a.fuzz, b.fuzz);
        assert_eq!(a.fuzz, 0.4);
        assert_eq!(b.fuzz, 0.8);
        assert_eq!(KnitBorder { fuzz: a.fuzz, ..b }, a);
    }

    #[test]
    fn zigzag_fuzz_and_zigzag_detail_differ_only_in_stitch_size() {
        let a = config(Scenario::KnitZigzagFuzz).knit.unwrap();
        let b = config(Scenario::KnitZigzagDetail).knit.unwrap();
        assert_ne!(a.stitch_size, b.stitch_size);
        assert_eq!(a.stitch_size, 8.);
        assert_eq!(b.stitch_size, 32.);
        assert_eq!(
            KnitBorder {
                stitch_size: a.stitch_size,
                ..b
            },
            a
        );
    }

    #[test]
    fn stats_single_sample() {
        let stats = frame_stats(&[us(7.)]).unwrap();
        assert_eq!(stats.count, 1);
        assert_close(stats.min_us, 7.);
        assert_close(stats.median_us, 7.);
        assert_close(stats.mean_us, 7.);
        assert_close(stats.p95_us, 7.);
        assert_close(stats.max_us, 7.);
    }

    #[test]
    fn stats_even_count() {
        let stats = frame_stats(&[us(4.), us(1.), us(3.), us(2.)]).unwrap();
        assert_eq!(stats.count, 4);
        assert_close(stats.min_us, 1.);
        assert_close(stats.median_us, 2.5);
        assert_close(stats.mean_us, 2.5);
        assert_close(stats.p95_us, 4.);
        assert_close(stats.max_us, 4.);
    }

    #[test]
    fn stats_odd_count() {
        let stats = frame_stats(&[us(1.), us(2.), us(3.), us(4.), us(5.)]).unwrap();
        assert_eq!(stats.count, 5);
        assert_close(stats.min_us, 1.);
        assert_close(stats.median_us, 3.);
        assert_close(stats.mean_us, 3.);
        assert_close(stats.p95_us, 5.);
        assert_close(stats.max_us, 5.);
    }

    #[test]
    fn stats_unsorted_input() {
        let stats = frame_stats(&[us(9.), us(1.), us(5.), us(3.)]).unwrap();
        assert_close(stats.min_us, 1.);
        assert_close(stats.median_us, 4.);
        assert_close(stats.mean_us, 4.5);
        assert_close(stats.p95_us, 9.);
        assert_close(stats.max_us, 9.);
    }

    #[test]
    fn stats_identical_values() {
        let stats = frame_stats(&[us(2.); 7]).unwrap();
        assert_eq!(stats.count, 7);
        assert_close(stats.min_us, 2.);
        assert_close(stats.median_us, 2.);
        assert_close(stats.mean_us, 2.);
        assert_close(stats.p95_us, 2.);
        assert_close(stats.max_us, 2.);
    }

    #[test]
    fn stats_p95_nearest_rank_for_20_samples() {
        let samples: Vec<Duration> = (1..=20).map(|v| us(v as f64)).collect();
        let stats = frame_stats(&samples).unwrap();
        assert_eq!(stats.count, 20);
        assert_close(stats.median_us, 10.5);
        assert_close(stats.mean_us, 10.5);
        assert_close(stats.p95_us, 19.);
    }

    #[test]
    fn stats_empty_input_is_error() {
        assert!(frame_stats(&[]).is_err());
    }

    #[test]
    fn stats_preserves_fractional_microseconds() {
        let stats = frame_stats(&[us(0.5)]).unwrap();
        assert_close(stats.min_us, 0.5);
        assert_close(stats.median_us, 0.5);
        assert_close(stats.mean_us, 0.5);
        assert_close(stats.p95_us, 0.5);
        assert_close(stats.max_us, 0.5);
    }

    // --- Multi-run aggregation (CPU-only) ---

    #[test]
    fn mean_frame_stats_averages_fields_and_sums_count() {
        let a = FrameStats {
            count: 60,
            min_us: 100.,
            median_us: 200.,
            mean_us: 210.,
            p95_us: 300.,
            max_us: 400.,
        };
        let b = FrameStats {
            count: 60,
            min_us: 110.,
            median_us: 220.,
            mean_us: 230.,
            p95_us: 320.,
            max_us: 420.,
        };
        let m = mean_frame_stats(&[a, b]).unwrap();
        assert_eq!(m.count, 120);
        assert_close(m.min_us, 105.);
        assert_close(m.median_us, 210.);
        assert_close(m.mean_us, 220.);
        assert_close(m.p95_us, 310.);
        assert_close(m.max_us, 410.);
    }

    #[test]
    fn mean_frame_stats_empty_is_error() {
        assert!(mean_frame_stats(&[]).is_err());
    }

    #[test]
    fn aggregate_runs_pools_samples_and_averages_stats() {
        let run_a = test_result(Scenario::Solid, Workload::Static, vec![100., 110.], None);
        let run_b = test_result(Scenario::Solid, Workload::Static, vec![120., 130.], None);
        let agg = aggregate_runs(vec![run_a, run_b]).unwrap();

        // Pooled samples preserve all runs' data.
        assert_eq!(agg.samples_us, vec![100., 110., 120., 130.]);
        // No statistics because inputs had none.
        assert!(agg.statistics.is_none());
        // Per-run data is kept for >1 runs.
        let runs = agg.runs.expect("multi-run result must carry runs");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].samples_us, vec![100., 110.]);
        assert_eq!(runs[1].samples_us, vec![120., 130.]);
        // Fence observations are summed.
        assert_eq!(agg.observed_synchronization.frames_with_fence, 4);
    }

    #[test]
    fn aggregate_runs_single_run_has_no_runs_field() {
        let run = test_result(Scenario::Solid, Workload::Static, vec![100.], None);
        let agg = aggregate_runs(vec![run]).unwrap();
        assert!(agg.runs.is_none());
    }

    #[test]
    fn aggregate_runs_averages_statistics_across_runs() {
        let stats_a = frame_stats(&[us(100.), us(200.)]).unwrap();
        let stats_b = frame_stats(&[us(300.), us(400.)]).unwrap();
        let run_a = test_result(
            Scenario::Solid,
            Workload::Static,
            vec![100., 200.],
            Some(stats_a),
        );
        let run_b = test_result(
            Scenario::Solid,
            Workload::Static,
            vec![300., 400.],
            Some(stats_b),
        );
        let agg = aggregate_runs(vec![run_a, run_b]).unwrap();
        let s = agg.statistics.unwrap();
        assert_eq!(s.count, 4);
        assert_close(s.min_us, (100. + 300.) / 2.);
        assert_close(s.max_us, (200. + 400.) / 2.);
    }

    // --- Report serialization (CPU-only; no EGL, GPU or real git) ---

    fn test_metadata() -> Metadata {
        Metadata {
            gl_vendor: Some("TestVendor".to_owned()),
            gl_renderer: Some("TestRenderer".to_owned()),
            gl_version: Some("OpenGL ES 3.2 test".to_owned()),
            package_version: env!("CARGO_PKG_VERSION"),
            build_version: "26.04 (test commit)".to_owned(),
            debug_assertions: cfg!(debug_assertions),
            target_size: [TARGET_SIZE.0, TARGET_SIZE.1],
            target_format: TARGET_FORMAT.to_string(),
            window_count: RING_COUNT,
            window_content_size: [WIN_SIZE.0, WIN_SIZE.1],
            border_width: BORDER_WIDTH,
            outer_corner_radius: OUTER_RADIUS,
            scale: SCALE,
            alpha: ALPHA,
            element_draws_per_frame: RING_COUNT * ELEMENTS_PER_RING,
            mode: "benchmark",
            warmup_frames: 30,
            measured_frames: 3,
            scenarios: vec!["solid"],
            revision_source: "runtime_checkout",
            checkout_head: Some("0123456789abcdef".to_owned()),
            checkout_dirty: Some(false),
            workloads: vec!["static"],
            resize: None,
            runs: 1,
        }
    }

    fn test_metadata_resize() -> Metadata {
        let mut metadata = test_metadata();
        metadata.workloads = vec!["static", "resize"];
        metadata.resize = Some(ResizeParams {
            min_window_size: [RESIZE_MIN_SIZE.0, RESIZE_MIN_SIZE.1],
            max_window_size: [WIN_SIZE.0, WIN_SIZE.1],
            cycle_steps: RESIZE_CYCLE_STEPS,
            easing: "cosine",
        });
        metadata
    }

    fn test_result(
        scenario: Scenario,
        workload: Workload,
        samples: Vec<f64>,
        stats: Option<FrameStats>,
    ) -> ScenarioResult {
        ScenarioResult {
            name: scenario.name(),
            workload: workload.name(),
            metric: "frame_wall_time_us",
            scope: "test scope",
            parameters: scenario_params(&scenario_config(scenario)),
            observed_synchronization: SyncObservations {
                frames_with_fence: samples.len(),
                frames_without_fence: 0,
            },
            samples_us: samples,
            statistics: stats,
            stages: None,
            runs: None,
        }
    }

    #[test]
    fn report_serializes_and_roundtrips() {
        let samples = vec![100.25, 101.5, 99.75];
        let durations: Vec<Duration> = samples.iter().map(|&s| us(s)).collect();
        let stats = frame_stats(&durations).unwrap();
        let report = Report {
            schema_version: 3,
            methodology_version: 3,
            metadata: test_metadata(),
            results: vec![test_result(
                Scenario::Solid,
                Workload::Static,
                samples,
                Some(stats),
            )],
        };

        let json = serde_json::to_string(&report).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(value["schema_version"], 3);
        assert_eq!(value["methodology_version"], 3);
        assert_eq!(value["metadata"]["mode"], "benchmark");
        assert_eq!(value["metadata"]["workloads"][0], "static");
        assert_eq!(value["results"].as_array().unwrap().len(), 1);
        assert_eq!(value["results"][0]["name"], "solid");
        assert_eq!(value["results"][0]["workload"], "static");
        assert_eq!(value["results"][0]["metric"], "frame_wall_time_us");
    }

    #[test]
    fn report_sample_count_matches_statistics_count() {
        let durations: Vec<Duration> = [100., 101., 102., 103.].iter().map(|&s| us(s)).collect();
        let stats = frame_stats(&durations).unwrap();
        let result = test_result(
            Scenario::Solid,
            Workload::Static,
            durations
                .iter()
                .map(|d| d.as_nanos() as f64 / 1000.)
                .collect(),
            Some(stats),
        );

        let json = serde_json::to_value(&result).unwrap();
        let samples_len = json["samples_us"].as_array().unwrap().len();
        assert_eq!(samples_len, 4);
        assert_eq!(json["statistics"]["count"], 4);
        assert_eq!(
            json["observed_synchronization"]["frames_with_fence"]
                .as_u64()
                .unwrap() as usize,
            samples_len
        );
    }

    #[test]
    fn report_preserves_fractional_microseconds() {
        let result = test_result(Scenario::Solid, Workload::Static, vec![100.5], None);
        let json = serde_json::to_value(&result).unwrap();
        assert_close(json["samples_us"][0].as_f64().unwrap(), 100.5);
    }

    #[test]
    fn smoke_result_has_empty_samples_and_null_statistics() {
        let mut result = test_result(Scenario::Solid, Workload::Static, vec![], None);
        result.observed_synchronization = SyncObservations {
            frames_with_fence: 1,
            frames_without_fence: 0,
        };

        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["samples_us"].as_array().unwrap().len(), 0);
        assert!(json["statistics"].is_null());
        assert_eq!(json["observed_synchronization"]["frames_with_fence"], 1);
    }

    #[test]
    fn unknown_git_and_gl_values_stay_null() {
        let mut metadata = test_metadata();
        metadata.gl_vendor = None;
        metadata.gl_renderer = None;
        metadata.gl_version = None;
        metadata.checkout_head = None;
        metadata.checkout_dirty = None;

        let json = serde_json::to_value(&metadata).unwrap();
        assert!(json["gl_vendor"].is_null());
        assert!(json["gl_renderer"].is_null());
        assert!(json["gl_version"].is_null());
        assert!(json["checkout_head"].is_null());
        // Unknown dirty status must not become a false "clean" claim.
        assert!(json["checkout_dirty"].is_null());
        assert_eq!(json["revision_source"], "runtime_checkout");
    }

    // --- Resize cycle (CPU-only; no EGL or GPU) ---

    #[test]
    fn resize_progress_hits_exact_cycle_extremes() {
        // The cycle must render the exact minimum at both ends and the exact
        // maximum in the middle — a drift here would silently skip geometry.
        assert_eq!(resize_progress(0), 0.);
        assert_eq!(resize_progress(RESIZE_CYCLE_STEPS / 2), 1.);
        assert_eq!(resize_progress(RESIZE_CYCLE_STEPS), 0.);
        // The cycle repeats identically.
        assert_eq!(
            resize_progress(RESIZE_CYCLE_STEPS + RESIZE_CYCLE_STEPS / 2),
            1.
        );
    }

    #[test]
    fn resize_progress_is_monotonic_within_half_cycles() {
        let mut prev = resize_progress(0);
        for step in 1..=RESIZE_CYCLE_STEPS / 2 {
            let p = resize_progress(step);
            assert!(p > prev, "step {step}: progress must grow to the max");
            prev = p;
        }
        for step in RESIZE_CYCLE_STEPS / 2 + 1..=RESIZE_CYCLE_STEPS {
            let p = resize_progress(step);
            assert!(p < prev, "step {step}: progress must shrink to the min");
            prev = p;
        }
    }

    #[test]
    fn resize_win_size_stays_inside_ring_slots() {
        // Every ring must fit its 548x1424 slot at every cycle step, or the
        // dst_fits_in_target check would fail mid-run.
        for step in 0..=RESIZE_CYCLE_STEPS {
            let size = resize_win_size(step);
            assert!(
                size.w + BORDER_WIDTH * 2. <= RING_STEP,
                "step {step}: width {} + borders exceeds slot {}",
                size.w,
                RING_STEP
            );
            assert!(
                size.h + BORDER_WIDTH * 2. <= TARGET_SIZE.1 as f64,
                "step {step}: height {} + borders exceeds target {}",
                size.h,
                TARGET_SIZE.1
            );
            // Corner segments need positive straight edges: win + 2*border
            // must exceed 2*outer_radius.
            assert!(
                size.w + BORDER_WIDTH * 2. > f64::from(OUTER_RADIUS) * 2.,
                "step {step}: width too small for corner radius"
            );
        }
    }

    #[test]
    fn resize_win_size_endpoints_match_min_and_max() {
        let min = resize_win_size(0);
        assert_close(min.w, RESIZE_MIN_SIZE.0);
        assert_close(min.h, RESIZE_MIN_SIZE.1);
        let max = resize_win_size(RESIZE_CYCLE_STEPS / 2);
        assert_close(max.w, WIN_SIZE.0);
        assert_close(max.h, WIN_SIZE.1);
        // Full cycle returns to the minimum.
        let end = resize_win_size(RESIZE_CYCLE_STEPS);
        assert_close(end.w, RESIZE_MIN_SIZE.0);
        assert_close(end.h, RESIZE_MIN_SIZE.1);
    }

    #[test]
    fn resize_metadata_reports_cycle_params() {
        let metadata = test_metadata_resize();
        let json = serde_json::to_value(&metadata).unwrap();
        assert_eq!(json["workloads"][1], "resize");
        let resize = &json["resize"];
        assert_eq!(resize["cycle_steps"], RESIZE_CYCLE_STEPS as u64);
        assert_eq!(resize["easing"], "cosine");
        assert_close(
            resize["min_window_size"][0].as_f64().unwrap(),
            RESIZE_MIN_SIZE.0,
        );
        assert_close(resize["max_window_size"][1].as_f64().unwrap(), WIN_SIZE.1);
    }

    #[test]
    fn resize_result_carries_stage_metrics() {
        let mut result = test_result(Scenario::Solid, Workload::Resize, vec![10.], None);
        result.stages = Some(StageMetrics {
            update_us: vec![3.],
            render_us: vec![7.],
            update_statistics: Some(frame_stats(&[us(3.)]).unwrap()),
            render_statistics: Some(frame_stats(&[us(7.)]).unwrap()),
        });
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["workload"], "resize");
        assert_close(json["stages"]["update_us"][0].as_f64().unwrap(), 3.);
        assert_close(json["stages"]["render_us"][0].as_f64().unwrap(), 7.);
        assert_eq!(json["stages"]["update_statistics"]["count"], 1);
    }

    #[test]
    fn static_result_has_null_stages() {
        let result = test_result(Scenario::Solid, Workload::Static, vec![10.], None);
        let json = serde_json::to_value(&result).unwrap();
        assert!(json["stages"].is_null());
    }

    #[test]
    fn scenario_params_match_scenario_config() {
        // knit-zigzag-detail: zigzag pattern, stitch_size 32, fuzz 0.8.
        let params = scenario_params(&scenario_config(Scenario::KnitZigzagDetail));
        let json = serde_json::to_value(&params).unwrap();
        assert_eq!(json["knit"]["pattern"], "zigzag");
        assert_close(json["knit"]["stitch_size"].as_f64().unwrap(), 32.);
        assert_close(json["knit"]["fuzz"].as_f64().unwrap(), 0.8);
        assert_close(json["knit"]["relief"].as_f64().unwrap(), 0.8);
        assert_eq!(
            json["focus_ring"]["active_gradient"],
            serde_json::Value::Null
        );
        assert_close(json["focus_ring"]["width"].as_f64().unwrap(), BORDER_WIDTH);

        // gradient-oklch: oklch color space, longer hue, no knit.
        let params = scenario_params(&scenario_config(Scenario::GradientOklch));
        let json = serde_json::to_value(&params).unwrap();
        assert_eq!(
            json["focus_ring"]["active_gradient"]["color_space"],
            "oklch"
        );
        assert_eq!(
            json["focus_ring"]["active_gradient"]["hue_interpolation"],
            "longer"
        );
        assert_eq!(json["focus_ring"]["active_gradient"]["angle"], 90);
        assert!(json["knit"].is_null());

        // solid: no gradient, no knit; colors round-trip as RGBA arrays.
        let params = scenario_params(&scenario_config(Scenario::Solid));
        let json = serde_json::to_value(&params).unwrap();
        assert!(json["knit"].is_null());
        assert!(json["focus_ring"]["active_gradient"].is_null());
        let color = json["focus_ring"]["active_color"].as_array().unwrap();
        assert_eq!(color.len(), 4);
        assert_close(color[3].as_f64().unwrap(), 1.);
    }

    // --- Git metadata interpretation (CPU-only; no repo, GPU or env state) ---

    #[test]
    fn git_output_preserves_successful_empty_stdout() {
        // Regression: a successful empty stdout used to be dropped before the
        // caller could interpret it, turning a clean status into "unknown".
        assert_eq!(interpret_git_output(true, b""), Some(String::new()));
        assert_eq!(interpret_git_output(true, b"  \n\t "), Some(String::new()));
    }

    #[test]
    fn git_output_trims_nonempty_stdout() {
        assert_eq!(
            interpret_git_output(true, b"  abc123\n"),
            Some("abc123".to_owned())
        );
    }

    #[test]
    fn git_output_failure_is_none() {
        assert_eq!(interpret_git_output(false, b""), None);
        assert_eq!(interpret_git_output(false, b"partial output"), None);
    }

    #[test]
    fn empty_status_means_clean_not_unknown() {
        // A successful empty status must reach Some(false); before the fix it
        // was swallowed into None by the helper.
        assert_eq!(
            interpret_status_output(interpret_git_output(true, b"")),
            Some(false)
        );
    }

    #[test]
    fn nonempty_status_means_dirty() {
        assert_eq!(
            interpret_status_output(interpret_git_output(true, b"?? examples/\n")),
            Some(true)
        );
    }

    #[test]
    fn failed_status_is_unknown_not_clean() {
        // A command failure must surface as None (unknown), never Some(false).
        assert_eq!(
            interpret_status_output(interpret_git_output(false, b"")),
            None
        );
        assert_eq!(interpret_status_output(None), None);
    }

    #[test]
    fn empty_head_stays_none() {
        assert_eq!(interpret_head_output(interpret_git_output(true, b"")), None);
        assert_eq!(
            interpret_head_output(interpret_git_output(true, b"abc123\n")),
            Some("abc123".to_owned())
        );
        assert_eq!(interpret_head_output(None), None);
    }

    // --- Element geometry validation (CPU-only; no EGL or GPU) ---

    fn target() -> Rectangle<i32, Physical> {
        Rectangle::from_size(Size::from(TARGET_SIZE))
    }

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Physical> {
        Rectangle::new(Point::from((x, y)), Size::from((w, h)))
    }

    #[test]
    fn dst_fully_inside_target_is_accepted() {
        assert!(dst_fits_in_target(rect(100, 100, 200, 300), target()));
    }

    #[test]
    fn dst_touching_target_edges_is_accepted() {
        let (w, h) = TARGET_SIZE;
        // Right and bottom edges may coincide with the target boundary.
        assert!(dst_fits_in_target(rect(w - 10, h - 20, 10, 20), target()));
        assert!(dst_fits_in_target(rect(0, 0, 10, 20), target()));
    }

    #[test]
    fn dst_equal_to_target_is_accepted() {
        assert!(dst_fits_in_target(target(), target()));
    }

    #[test]
    fn dst_nonpositive_size_is_rejected() {
        for (w, h) in [(0, 10), (10, 0), (0, 0), (-1, 10), (10, -1), (-5, -5)] {
            // Size::new debug-asserts non-negative values; write the fields
            // directly to exercise the guard against a broken invariant.
            let mut dst = rect(10, 10, 1, 1);
            dst.size.w = w;
            dst.size.h = h;
            assert!(
                !dst_fits_in_target(dst, target()),
                "size {w}x{h} must be rejected"
            );
        }
    }

    #[test]
    fn dst_partially_outside_target_is_rejected() {
        let (w, h) = TARGET_SIZE;
        for dst in [
            rect(-1, 0, 10, 10),    // left
            rect(0, -1, 10, 10),    // top
            rect(w - 5, 0, 10, 10), // right
            rect(0, h - 5, 10, 10), // bottom
        ] {
            assert!(
                !dst_fits_in_target(dst, target()),
                "dst {dst:?} must be rejected"
            );
        }
    }

    #[test]
    fn dst_fully_outside_target_is_rejected() {
        let (w, h) = TARGET_SIZE;
        for dst in [
            rect(-100, 0, 10, 10),
            rect(0, -100, 10, 10),
            rect(w + 10, 0, 10, 10),
            rect(0, h + 10, 10, 10),
        ] {
            assert!(
                !dst_fits_in_target(dst, target()),
                "dst {dst:?} must be rejected"
            );
        }
    }
}
