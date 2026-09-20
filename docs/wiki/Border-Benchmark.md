# Border Benchmark

`border_bench` is a standalone benchmark that renders a fixed border scene into an offscreen texture and measures per-frame wall time. It exists to compare border rendering changes on the same machine, not to produce absolute GPU numbers.

Run it from the repository root:

```bash
cargo bench --locked -p niri --bench border_bench -- --scenario all
```

## What is measured

The metric is `frame_wall_time_us`: the wall-clock time of one complete frame, measured on the CPU around the draw call sequence. Each frame covers:

- `renderer.render()` — starting the frame;
- `clear` of the whole target;
- 48 `RenderElement::draw` calls (6 windows × 8 border segments, all shader-backed);
- `finish` — submitting the frame;
- completion wait — blocking until the GPU confirms the frame is done.

The measurement does **not** include:

- renderer and shader creation;
- geometry, config and uniform preparation;
- binding the render target for the series;
- framebuffer readback;
- PNG encoding;
- JSON serialization and result output.

This is not pure GPU time, not the cost of a single border segment, and not the time of a full compositor frame. The scene is prepared once and stays static, so the benchmark does not measure resize, config updates, geometry recomputation, CPU precomputation on parameter changes, or full desktop compositing.

Frame completion is guaranteed by Smithay: `finish()` returns a `SyncPoint` backed by an `EGLFence` when the driver supports it, and falls back to `glFinish` internally otherwise. The report records how many measured frames returned a fence (`frames_with_fence` / `frames_without_fence`); a frame without a fence is still synchronized.

## How to run

Smoke test — one real frame per scenario, no timing:

```bash
cargo bench --locked -p niri --bench border_bench -- --smoke --scenario all
```

Smoke test with PNG dumps of the rendered frames (saved to `target/border_bench/`):

```bash
cargo bench --locked -p niri --bench border_bench -- \
  --smoke --scenario all --dump-dir
```

A single scenario:

```bash
cargo bench --locked -p niri --bench border_bench -- \
  --scenario knit-zigzag
```

All scenarios (the default when `--scenario` is omitted):

```bash
cargo bench --locked -p niri --bench border_bench -- --scenario all
```

Benchmark with an explicit frame count:

```bash
cargo bench --locked -p niri --bench border_bench -- \
  --scenario all --warmup 30 --frames 300
```

JSON report into a file (stdout carries exactly one JSON document; diagnostics go to stderr):

```bash
cargo bench --locked -p niri --bench border_bench -- \
  --scenario all --json > /tmp/border_bench.json
```

CPU-only tests (no GPU, no EGL, no git access needed):

```bash
cargo test --locked -p niri --test border_bench_tests
```

### Visual regression testing (golden images)

When optimizing knit shaders or border rendering paths, always verify that the rendering results match the golden reference images:

```bash
cargo test knit
```

Golden tests require deterministic rendering via Mesa's software `llvmpipe` renderer. If running on a system where hardware EGL is the default:

```bash
__EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json cargo test knit
```

If a test fails due to pixel differences exceeding 1%:
- A newly rendered image `<test_name>.new.png` is generated directly in `src/tests/golden/`.
- Compare `src/tests/golden/<test_name>.png` with `src/tests/golden/<test_name>.new.png` to review the visual discrepancy.
- If the visual change is intentional, update the reference images with `NIRI_GOLDEN_UPDATE=1 cargo test knit`.

Warmup frames run before the measured series so that one-time costs — shader warm-up, allocator caches, driver state — do not pollute the samples. They are drawn but never timed. Single runs are noisy: repeat the benchmark several times and compare medians, not individual runs.

Scenario names: `solid`, `gradient-srgb`, `gradient-oklch`, `knit-stockinette`, `knit-zigzag`, `knit-zigzag-fuzz`, `knit-zigzag-detail`.

## Reading results

All times are in microseconds. The table and the JSON report show `min`, `median`, `mean`, `p95` and `max` over the raw samples.

- **median** is the middle value of the sorted samples; it is the most robust single number for comparisons.
- **p95** uses the nearest-rank method: the sample at index `ceil(0.95 × N) − 1` in the sorted array. It describes the slow tail, not an average.
- Raw `samples_us` are kept in full — no outliers are removed, so a slow first frame or a scheduling hiccup stays visible in `max` and `p95`.

A short trial run does not prove a small improvement. If the change you are testing is within a few percent, run more frames and repeat the comparison.

The JSON report also records run metadata: GL vendor/renderer/version strings, package and build versions, the `debug_assertions` flag, target size and format, workload parameters, and the runtime checkout state. `GL_RENDERER` is the driver-reported renderer string, not proof of a specific physical GPU. `debug_assertions = false` reports the flag itself, not a specific Cargo profile. `element_draws_per_frame` counts `RenderElement::draw` calls, not low-level GL draw calls.

The checkout fields come from `revision_source = "runtime_checkout"`: they describe the working tree at run time and do not guarantee that the binary was built from that HEAD. The build-time revision is reported separately in `build_version`.

A software renderer (llvmpipe and similar) is fine for smoke tests and correctness checks, but its numbers must not be presented as a hardware GPU benchmark.

## Before / after comparison

To compare a change, build the same benchmark harness on both revisions and alternate runs on one machine.

Set up two worktrees:

```bash
git worktree add ../niri-before <old-commit>
git worktree add ../niri-after <new-commit>
```

Conditions for a fair comparison:

- identical benchmark harness source on both sides;
- identical scenarios and parameters;
- one machine, same driver and renderer;
- same Rust toolchain, build profile and features;
- a separate build and a separate target directory per checkout;
- sequential runs, never parallel — two benchmarks competing for the GPU invalidate each other;
- several alternating A/B runs, not a single pair;
- keep the JSON report of every run: it stores the metadata and samples needed to check that the conditions above actually held.

If the old commit has no `border_bench` benchmark, port only the harness — do not backport production changes. If the old API is incompatible with the current harness, do not patch production code just to make the comparison work; record the required harness adaptation separately and verify it.

The older C benchmark used a different rendering path and an RGB565 target. Its absolute numbers are not acceptance criteria for the current Abgr8888 harness.
