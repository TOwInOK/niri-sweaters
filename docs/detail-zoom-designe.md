# Desktop Zoom — Technical Design

Canonical design document for the desktop zoom feature. It is the contract for
the upcoming stages: `toggle-zoom`, `hold-zoom`, smooth level animation,
Zoom ↔ Overview interaction, and pinch.

Each section is marked:

- **Implemented** — describes the code as it exists today
  (`src/layout/zoom.rs`, `src/utils/view.rs`, `src/input/mod.rs`,
  `src/niri.rs`, `niri-config`, `niri-ipc`).
- **Decided** — an accepted UX/architecture decision not yet implemented;
  binding for the next stages.
- **Open** — deliberately deferred; do not treat as decided.

## Overview

Desktop zoom is a compositor-side magnifier: the already-rendered desktop scene
of an output is scaled around a focal point. It is not a DPI mechanism —
clients keep their scale and never re-render at a higher resolution. Image
blur at high zoom levels is acceptable; filtering/sharpening may be improved
separately later.

Use output/UI scale for permanent UI sizing; use desktop zoom for temporary
magnification of the rendered scene.

Canonical user flow:

1. The user points at the area of interest.
2. `toggle-zoom`/`hold-zoom` enters a zoom preset.
3. The zoom animates smoothly around the cursor.
4. Wheel/actions change the zoom target.
5. The deadzone lets the user work inside the zoomed desktop.
6. The Overview temporarily returns the presentation to normal scale.
7. Toggle/release/reset returns the zoom according to the chosen lifecycle
   semantics.

## Coordinate model — Implemented

`ViewportTransform` (`src/utils/view.rs`) is the canonical primitive mapping
between content and displayed coordinates in output-local logical space:

```text
display = focal + (content − focal) × factor
```

`focal` is the fixed point of the transform (`apply(focal) == focal`). The API
is `apply`, `apply_inverse`, `apply_rect`, `apply_inverse_rect`, `identity`.

The canonical pointer position is always a **content** coordinate. Displayed
positions exist only at the input-mapping boundary and at render time.

## State — Implemented

`OutputZoomState` (`src/layout/zoom.rs`) lives on the `Monitor`, one per
output:

```rust
pub struct OutputZoomState {
    level: f64,                  // committed, currently displayed zoom
    target_level: f64,           // user-requested level (user intent)
    focal: Point<f64, Logical>,  // transform fixed point, output-local
    locked: bool,                // disables focal tracking
}
```

- `level` and `target_level` are separate fields. Today every action goes
  through `set_level_immediate`, which sets both; the split exists so that
  continuous actions and future animations operate on `target_level` while
  `level` remains the displayed value.
- The logical viewport is `view_size / level` and is always contained within
  the output: `clamp_focal` keeps `0 ≤ focal ≤ view_size` per axis.
- `update_view_size` preserves the focal when it stays valid, clamps it
  otherwise, and resets it to the output center at `level == 1`.

## Rendering — Implemented

- The desktop scene is wrapped in `RescaleRenderElement` with the zoom origin
  at `focal` (physical) and the committed `level` as the factor
  (`push_desktop!` in `render_inner`, `src/niri.rs`).
- At `level == 1` elements are pushed without a wrapper — identity fast path.
- Applies uniformly to all render targets: `Output`, `Screencast`,
  `ScreenCapture` (covered by `zoom_applies_to_all_render_targets`).
- The pointer sprite is **not** scaled; only its position is transformed
  (`render_pointer_with_transform`). The visual hotspot is the displayed
  position of the canonical content position.
- No native client rerender is triggered; clients are unaware of the zoom.

## Input model — Implemented

All mapping goes through the committed `viewport_transform()` of the relevant
output.

- **Relative pointer:** the canonical pointer position is in content
  coordinates; device deltas are divided by the committed zoom level of the
  output under the pointer (`delta /= level`).
- **Absolute pointer:** `display → viewport inverse → content`
  (`absolute_location_from_display`).
- **Touch:** uses the same display→content mapping, but never moves the focal
  point.
- **Tablet:**
  - output mapping: `display → viewport inverse → content`;
  - focused-window mapping: `device → logical window content rect directly`,
    with no inverse zoom — the target rectangle is already content geometry,
    so zoom must not be applied twice.
- **Warps** (`move_cursor`, pointer-constraint position hints): the warp
  target is a canonical content coordinate. Unlocked: the focal syncs
  immediately via `update_zoom_focal_for_cursor`. Locked: the pointer is
  clamped to the current viewport (`clamp_pointer_to_locked_viewport`). A warp
  is a teleport — it produces no pointer motion and no animation step.
- **Locked zoom** at `level > 1` clamps the pointer to the current viewport.
- **Hot corners** are a display-space concept: the canonical position is
  transformed to its displayed position before the corner test.

## Deadzone — Implemented

```kdl
zoom {
    max-zoom 10.0
    increment-factor 1.2
    deadzone-size 0.5
}
```

`deadzone-size` is the fraction of the output size along each axis, always
centered on the output:

- `0` → degenerates to the center point: centered tracking;
- `1` → covers the whole output: edge push.

While the displayed cursor stays inside the deadzone the focal does not move.
Once it leaves, the focal moves just enough to bring the displayed cursor back
to the deadzone edge. **Viewport bounds take priority over the deadzone**: if
following the cursor would push the viewport outside the output, the viewport
stays clamped and the cursor may remain outside the deadzone.

**Decided:** deadzone tracking stays immediate — the focal reacts the moment
the cursor leaves the box. No camera inertia/smoothing is added.

## Actions — Implemented

```text
zoom-in
zoom-out
set-zoom-level <LEVEL>
reset-zoom
toggle-zoom-lock
```

- `zoom-in`/`zoom-out` multiply/divide **`target_level`** by
  `increment-factor` — the semantic base is user intent, so they stay correct
  once transitions are animated. The result is clamped to `[1, max-zoom]` and
  snapped to exactly `1` within `1e-9` so repeated in/out cycles leave no
  residue.
- `set-zoom-level` validates `level ≥ 1` and clamps to `max-zoom`.
- `reset-zoom` is `set-zoom-level 1`.
- `toggle-zoom-lock` toggles focal tracking.
- Target output: the output under the canonical pointer position, or the
  active output when the pointer is on no output. Anchor: the pointer position
  on the target output, or the output center otherwise.
- All actions currently apply through `set_level_immediate`: `level` jumps to
  the new value and the focal is recomputed so the anchor keeps its displayed
  position where the output bounds allow. This immediacy is the current
  implementation state, not the final UX semantics — see Smooth level
  animation.
- IPC exposes the same actions (`niri-ipc`: `ZoomIn`, `ZoomOut`,
  `SetZoomLevel`, `ResetZoom`, `ToggleZoomLock`).
- Config reload: lowering `max-zoom` clamps `level` and `target_level` of
  every output immediately (the lock does not exempt an output); other zoom
  settings only affect future actions and tracking.

## `toggle-zoom <LEVEL>` — Decided

New bind action:

```kdl
Mod+Z { toggle-zoom 2.0; }
```

`LEVEL` belongs to the action/bind. There is **no** global `activation-level`
in `zoom {}` — preset levels live on binds, so multiple presets need no extra
config:

```kdl
Mod+Z       { toggle-zoom 2.0; }
Mod+Shift+Z { toggle-zoom 4.0; }
```

The toggle decision is made on **user intent**, i.e. `target_level`, never on
the animated `level`:

```text
if target_level > 1:
    new target = 1
else:
    new target = LEVEL
```

This is critical once animations exist: pressing toggle mid-animation must
still do what the user means.

```text
1× → toggle-zoom 2 → 2×
   → zoom-in → 2.4× → zoom-in → 2.88×
   → toggle-zoom 2 → 1×
   → toggle-zoom 2 → 2×
```

The preset defines the **entry level**; it does not pin the zoom session —
further zoom actions move the target freely.

## `hold-zoom <LEVEL>` — Decided

New bind behavior: momentary zoom driven by the press/release lifecycle.

```kdl
Mod+X { hold-zoom 2.0; }
```

- **Press:** resolve the target output, save the previous zoom state
  (displayed level, `target_level`, focal), then animate towards `LEVEL`
  like a regular zoom action around the action anchor.
- **While held:** `zoom-in`, `zoom-out`, `set-zoom-level`, `toggle-zoom`,
  `reset-zoom` and deadzone tracking keep working normally on that output.
- **Release:** animate the viewport back to the pre-hold state. This is
  **not** reset-to-1:

```text
1×   → hold-zoom 2 → 2×   → wheel → 2.88× → release → 1×
1.5× → hold-zoom 3 → 3×   → wheel → 3.6×  → release → 1.5×
```

- **Output ownership:** the output is determined at press and owned for the
  whole hold session. If the cursor moves to another output while held,
  release still restores the original output. The output is never re-resolved
  at release.
- **Re-entry:** a second hold press on the same output replaces the session
  but keeps the original pre-hold snapshot — no intermediate restore. A press
  on another output ends the old session with an animated restore first.
- **Cleanup:** lost-release paths (VT switch, suspend, device removal, winit
  focus loss) restore the saved state immediately, without animation.
- **Lock:** `locked` is not part of the snapshot. While locked, the release
  animates only the level back; the focal point stays fixed.

Runtime state:

```rust
struct ZoomHoldState {
    trigger: ZoomHoldTrigger, // physical key/button owning the release
    output: Output,
    previous: ZoomSnapshot,   // displayed level, target_level, focal
}
```

The release runs a dedicated `ZoomLevelTransition::Restore`: a progress
animation `0 → 1` on the `animations.zoom` config, with the level moving in
`log2` space and the focal point interpolating towards the saved focal
(clamped to the current output size). The old `Animation` object is never
resumed; a new one is built from the displayed state, carrying the current
log-space velocity over as `v_log / Δz`. User input during the restore
(deadzone tracking, warps, new zoom actions, locking) takes over the camera
and converts the restore into a regular level transition.

## Smooth level animation — Decided

Zoom level must not visually jump. Canonical semantics:

```text
target_level = user intent
level        = current displayed zoom
```

Actions change `target_level`; an animation drives `level → target_level`.

- **Retargeting:** a new zoom input does not wait for the previous animation
  and does not queue. The visual level moves continuously toward the latest
  target — the running animation is restarted from its current value and
  velocity toward the new target (`Animation::restarted` semantics).

```text
target 1.0 → 1.2 → 1.44 → 1.728   (level follows continuously)
```

- **Transition model:**

```rust
enum ZoomLevelTransition {
    Idle,
    Animating(Animation),
    Gesturing(/* … */),  // reserved for pinch
}
```

- Uses the existing niri animation framework (`crate::animation`:
  `Animation`, `Clock`). No new animation engine.
- **Config:** through the existing `animations {}` block, preferred shape:

```kdl
animations {
    zoom {
        spring damping-ratio=1.0 stiffness=800 epsilon=0.0001
    }
}
```

  The exact node name is finalized at implementation in the style of the
  existing animation options (`workspace-switch`, `horizontal-view-movement`,
  …). Spring constants are **Open**.
- **Cursor anchor:** during a level transition the displayed anchor (normally
  the cursor) must not jump. For each animated `level`, the focal is computed
  so that `transform(anchor)` keeps its previous displayed position, unless
  the output clamp makes that impossible — the same math
  `set_level_immediate` already uses, applied per frame.
- **Not animated:**
  - deadzone tracking — the focal reacts immediately when the cursor leaves
    the box; no camera inertia;
  - programmatic warps — a warp is a teleport; the focal repositions
    immediately.

## Zoom ↔ Overview — Decided

Zoom and Overview are never simultaneously fully-active presentation
transforms:

```text
          NORMAL
         /      \
      ZOOM     OVERVIEW
```

- **Zoom → Overview:** the stored zoom state is preserved. Effective zoom
  animates `current → 1` while overview progress animates `0 → 1` — a single
  visual motion with no stop at Normal.
- **Overview → Zoom:** overview progress `1 → 0` while effective zoom
  `1 → stored/current zoom`. The stored state is not destroyed.
- **Zoom actions during Overview:** they change the stored `target_level`
  while the effective presentation stays Overview/1×. On exit, the zoom
  returns to the current target.

Note on current code: today the desktop zoom transform wraps the already
overview-scaled scene (multiplicative composition). The suppression model
above replaces that composition; the transition is future work.

## Pinch — Future work

Reserved via the `Gesturing` transition variant. Finger count, tracking
semantics, and gesture lifecycle are **Open**.

## Explicitly undecided — Open

Do not treat any of these as decided:

- default keybindings;
- whether `Mod+Wheel` zooming exists;
- exact spring constants;
- pinch finger count;
- filtering threshold and sharpening shader;
- IPC response schema for zoom state;
- exact press/release implementation of `hold-zoom` in the bind parser;
- exact runtime storage location of `ZoomHoldState`.

## Tests — Implemented

Behavior is covered by unit tests in `src/layout/zoom.rs` and
`src/utils/view.rs`, and by integration tests in `src/tests/zoom.rs`
(render targets, geometry, damage, pointer/tablet/touch mapping, warps,
deadzone, lock, actions, config reload).
