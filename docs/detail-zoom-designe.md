# Desktop Zoom — Technical Design

Canonical design document for the desktop zoom feature. It describes the
architecture as implemented in `src/layout/zoom.rs`, `src/utils/view.rs`,
`src/input/mod.rs`, `src/niri.rs`, `src/layout/monitor.rs`,
`src/ui/zoom_debug.rs`, `niri-config`, and `niri-ipc`.

User-facing behavior is documented on the wiki
(`docs/wiki/Accessibility.md`, desktop zoom section); this file is the
developer contract.

## Overview

Desktop zoom is a compositor-side magnifier: the already-rendered desktop
scene of an output is scaled around a focal point. It is not a DPI
mechanism — clients keep their scale and never re-render at a higher
resolution. Image blur at high zoom levels is acceptable; filtering or
sharpening may be improved separately later.

Use output/UI scale for permanent UI sizing; use desktop zoom for temporary
magnification of the rendered scene.

## Event flow

```text
input/action
    → zoom domain operation
    → OutputZoomState transition
    → presentation transform
    → render / pointer mapping
```

Wayland input, IPC actions and rendering are adapters around the zoom domain
state; none of them own a copy of it.

## Coordinate spaces

Three spaces are involved; the names below are used consistently throughout:

- **Content** (canonical): output-local logical coordinates of the desktop
  scene. The canonical pointer position is always a content coordinate.
- **Displayed**: output-local logical coordinates on screen after the zoom
  transform. The deadzone rectangle, the pointer's displayed position and
  `Anchored::display` live here.
- **Global logical**: the compositor-wide layout space; output-local points
  are offset by the output's geometry origin.

`ViewportTransform` (`src/utils/view.rs`) is the canonical primitive mapping
between content and displayed coordinates for one output:

```text
display = focal + (content − focal) × factor
```

`focal` is the fixed point of the transform (`apply(focal) == focal`), so its
value is identical in content and displayed space. The API is `apply`,
`apply_inverse`, `apply_rect`, `apply_inverse_rect`, `identity`.

At `level == 1` the transform is the identity: the focal point is visually
degenerate, but the stored focal is still kept as state (see "Zooming to 1×").

## State

`OutputZoomState` (`src/layout/zoom.rs`) lives on the `Monitor`, one per
output, and is the single authoritative owner of the zoom state:

```rust
pub struct OutputZoomState {
    level: f64,                  // committed, currently displayed zoom
    target_level: f64,           // user-requested level (user intent)
    focal: Point<f64, Logical>,  // transform fixed point, output-local
    locked: bool,                // disables pointer-driven camera following
    view_size: Size<f64, Logical>,
    transition: Option<ZoomLevelTransition>,
}
```

- `level` and `target_level` are separate: actions change `target_level`, a
  transition drives `level` towards it. Incremental actions (`zoom-in`,
  `zoom-out`, `toggle-zoom`) operate on `target_level` so they stay correct
  mid-animation.
- The logical viewport is `view_size / level` and is always contained within
  the output: `clamp_focal` keeps `0 ≤ focal ≤ view_size` per axis.
- `update_view_size` preserves the focal when it stays valid, clamps it
  otherwise, and resets it to the output center at `level == 1`.

## Transitions

All transient presentation and lifecycle state lives in the transition enum;
the committed fields are only updated when a transition completes:

- **`Animation`** — a zoom-level transition driven by an `Animation` over
  `log2(level)`, so equal multiplicative steps (1×→2× and 2×→4×) cover equal
  animation distance. Carries a focal mode:
  - `Anchored { content, display }` — keeps the `content` point at the
    `display` position while the level animates. Used for unlocked zoom
    around the pointer and for locked zoom around the viewport center.
  - `Fixed { focal }` — the focal point stays fixed. Used while locked and
    for zoom-out to 1×.

  May additionally carry a `display_follow` timestamp: while the displayed
  cursor is outside the deadzone and the transition zooms in, the anchor's
  display position drifts towards the deadzone boundary with the shared
  follow physics — the camera starts following during the zoom animation,
  not after it.
- **`Restore`** — returns the viewport to a saved `ZoomSnapshot` after a
  `hold-zoom` release: a progress animation `0 → 1` with the level in `log2`
  space and the focal interpolating linearly. Owns both level and focal.
- **`Gesturing`** — a pinch gesture owns the level directly: gesture updates
  set the displayed level, with no clock involvement.
- **`Follow`** — a resting deadzone follow: the level is already at its
  target and only the focal moves, driven by the shared distance-driven
  physics. Carries its own `last_step` timestamp.

A transition owns the viewport exclusively: a `Follow` never runs while an
`Animation`, `Restore` or `Gesturing` is active. The combined drift inside
`Animation` is the follow's share of that case, so zoom and follow can still
proceed as one continuous movement — the drift may finish before the level
animation, or outlive it and continue seamlessly as a resting `Follow`.

### Retargeting

A new zoom input does not wait for the previous animation and does not queue:
the running animation is restarted from the current displayed value towards
the new target, preserving velocity where the animation kind supports it
(springs carry velocity; easings retarget position-continuously).

### Zooming to 1×

Architecture rule: transitions to exactly 1× must not solve the anchored
transform for the focal point. The solve divides by `level − 1` and
degenerates as the level approaches 1, which would send the focal flying to
a clamped corner and pan the viewport on the way out.

- `set_target_level(1)` uses `Fixed` focal: the zoom-out pivots around the
  current focal's displayed position and lands continuously.
- `Restore` to 1× keeps `from_focal` until completion: the destination focal
  is degenerate at the identity transform, so interpolating towards it early
  would only pan the viewport.
- A restore takeover that targets 1× converts `Anchored` to `Fixed` for the
  same reason.
- `zoom-in`/`zoom-out` results and pinch updates snap the level to exactly 1
  within `ZOOM_SNAP_TO_ONE_EPSILON`, so repeated cycles leave no
  floating-point residue that keeps the zoom nominally active.

## Rendering

- The desktop scene is wrapped in `RescaleRenderElement` with the zoom origin
  at `focal` (physical) and the displayed `level` as the factor
  (`push_desktop!` in `render_inner`, `src/niri.rs`).
- At `level == 1` elements are pushed without a wrapper — identity fast path.
- Applies uniformly to all render targets: `Output`, `Screencast`,
  `ScreenCapture` (covered by `zoom_applies_to_all_render_targets`).
- The pointer sprite is **not** scaled; only its position is transformed
  (`render_pointer_with_transform`). The visual hotspot is the displayed
  position of the canonical content position.
- No native client rerender is triggered; clients are unaware of the zoom.

## Input model

All mapping goes through the presentation transform of the relevant output
(`pointer_presentation_transform`: the effective zoom transform normally, the
identity while the session is locked). The canonical pointer position is
always a content coordinate; displayed positions exist only at the
input-mapping boundary and at render time. Never inverse-transform an
already-canonical point.

- **Relative pointer:** device deltas are divided by the displayed zoom level
  of the output under the pointer (`delta /= level`).
- **Absolute pointer:** `display → viewport inverse → content`
  (`absolute_location_from_display`).
- **Touch:** same display→content mapping, but never moves the focal point.
- **Tablet:**
  - output mapping: `display → viewport inverse → content`;
  - focused-window mapping: `device → logical window content rect directly`,
    with no inverse zoom — the target rectangle is already content geometry,
    so zoom must not be applied twice.
- **Warps** (`move_cursor`, pointer-constraint position hints): the warp
  target is a canonical content coordinate — a teleport of the pointer, not
  of the camera. While locked, the target is first clamped to the currently
  presented viewport (`prepare_zoom_warp_target` →
  `clamp_pointer_to_locked_viewport`). While unlocked, the warped position is
  then evaluated like any other pointer position: if it lands outside the
  deadzone, a follow starts and brings it back to the deadzone edge over
  time.
- **Locked zoom** at `level > 1` clamps the pointer to the currently
  presented viewport on every motion.
- **Hot corners** are a display-space concept: the canonical position is
  transformed to its displayed position before the corner test.

## Deadzone and continuous follow

`deadzone-size` is the fraction of the output size along each axis, always
centered on the output: `0` degenerates to the center point (a classic
centered magnifier), `1` covers the whole output (follow effectively
disabled).

While the displayed cursor stays inside the deadzone the camera does not
move. Once it leaves, a **time-based** follow runs every frame:

- the viewport keeps moving even if the physical pointer stops;
- it slows down as the displayed cursor approaches the deadzone boundary;
- it ends when the cursor is visually back at the boundary, or when the
  output clamp makes further correction impossible.

**Viewport bounds take priority over the deadzone**: if following the cursor
would push the viewport outside the output, the viewport stays clamped and
the cursor may remain outside the deadzone. The follow then terminates at
the reachable clamped focal rather than running forever.

### Distance-driven physics

Computed per frame from the current displayed geometry only — no momentum or
carried velocity:

```text
P = displayed pointer position
D = nearest point inside/on the deadzone
O = P − D                        (overshoot vector)
```

Per-axis normalized depth `nx`, `ny` is `|O.axis| / available`, where
`available` is the space from the crossed deadzone border to the output edge
in the overshoot direction. Intensity is the relative `L∞` depth:

```text
t = max(nx, ny)
```

Direction is `normalize(O)` — deliberately separate from intensity: the same
relative depth produces the same scalar speed whether the overshoot is
horizontal or diagonal, so diagonal movement gets no `√2` boost.

Speed is interpolated between `follow-min-speed` and `follow-max-speed`
(displayed logical pixels per second; defaults 80 and 1400) by the
intensity — currently through a smoothstep curve, which is an implementation
detail rather than a public contract. The per-frame step is capped at the
remaining distance to the target, so a large `dt` can never overshoot the
target, produce NaN, or cross the deadzone border.

### Timing ownership

Follow timing belongs to the specific active follow operation:

- a resting `Follow` owns its `last_step` timestamp — created with the
  follow, destroyed with it;
- the combined drift inside `Animation` owns its own `display_follow`
  timestamp.

Nothing consumes time that elapsed while tracking was suspended or while
another transition owned the viewport: suspension (`suspend_follow`) commits
a resting follow at its current displayed position and drops the drift
marker, so a later resume re-seeds from live geometry with `dt = 0`.

Tracking is suspended while the session is locked, while the target output's
Overview is active, and while the screenshot UI or the recent-windows (MRU)
UI is visually active (`zoom_tracking_enabled`). The per-frame driver
(`Niri::update_zoom_follow`, called from `advance_animations`) evaluates the
pointer-owning output and commits follows on every other output.

## Actions

```text
zoom-in / zoom-out
set-zoom-level <LEVEL>
reset-zoom
zoom-lock [hold=true]
toggle-zoom <LEVEL> [hold=true]
hold-zoom <LEVEL> [hold=true]
```

- `zoom-in`/`zoom-out` multiply/divide `target_level` by `increment-factor`,
  clamped to `[1, max-zoom]` and snapped to exactly 1 within `1e-9`.
- `set-zoom-level` validates `level ≥ 1` and clamps to `max-zoom`;
  `reset-zoom` is `set-zoom-level 1`.
- `zoom-lock` toggles the lock; `zoom-lock hold=true` inverts it only while
  the trigger is held and restores the previous state on release.
- `toggle-zoom <LEVEL>` decides on `target_level` (user intent): above 1 →
  back to 1, otherwise → `LEVEL`. `hold=true` additionally locks while zoomed
  in and unlocks when toggling back to 1×.
- `hold-zoom <LEVEL>` is a momentary zoom (see below); `hold=true` also locks
  during the hold and restores the previous lock state on release.
- `hold-zoom` and `zoom-lock hold=true` are bind-only: their semantics depend
  on the physical press/release lifecycle, so they are not exposed through
  `niri msg action`. Scroll binds cannot drive them (no release event).
- Target output: the output under the canonical pointer position, or the
  active output when the pointer is on no output. Anchor: the pointer
  position on the target output, or the output center otherwise.
- IPC exposes `ZoomIn`, `ZoomOut`, `SetZoomLevel`, `ResetZoom`, `ZoomLock`
  and `ToggleZoom` (with `hold`).
- Config reload: lowering `max-zoom` clamps `level` and `target_level` of
  every output immediately (the lock does not exempt an output); other zoom
  settings only affect future actions and tracking.

## Zoom lock

`locked` disables pointer-driven camera following — the viewport becomes
independent of pointer movement, and the pointer is clamped to the presented
viewport.

The user-facing anchor invariant while locked is **the viewport center**, not
the focal point: changing the level keeps the content currently displayed at
the center of the viewport centered there (an `Anchored` transition on the
viewport center). The focal point itself moves as needed.

Locking mid-transition materializes the current displayed camera position
first (a running follow commits its focal), drops any deadzone drift, and
switches the transition to locked presentation ownership — no camera jump.
Unlocking does not move the viewport; the next evaluation may start a fresh
follow from the current geometry.

## `hold-zoom` and `Restore`

Press resolves the target output, snapshots the current view (displayed
level, `target_level`, focal — `locked` is deliberately not part of the
snapshot), then animates towards `LEVEL` like a regular zoom action.

While held, normal zoom interactions keep working on that output: zoom
actions, deadzone tracking, lock toggles. The output is owned for the whole
session — release restores the original output even if the pointer moved
elsewhere.

Release runs a `Restore`: the level animates in `log2` space towards the
saved `target_level` while the focal interpolates towards the saved focal —
the whole viewport returns to the pre-hold state, not just to 1×. While
locked, only the level animates back; the focal stays fixed.

A `Restore` owns both level and focal: no `Follow` runs concurrently. User
input during a restore (pointer tracking, warps, new zoom actions, locking)
takes over the camera: the restore converts into a regular level `Animation`
from the current displayed state, keeping the level's velocity, and the saved
focal destination is abandoned.

Re-entry replaces the session rather than nesting: a second press on the same
output keeps the original pre-hold snapshot; a press on another output ends
the old session with an animated restore first. Lost-release paths (VT
switch, suspend, device removal, focus loss) restore the saved state
immediately, without animation.

## Pinch

`zoom.pinch-fingers` opts in: a pinch begin with exactly the configured
finger count is claimed for the compositor (`zoom_pinch_claim_allowed`
requires no session lock, no screenshot UI or MRU, no pointer grab, and no
active Overview on the target output). The claim is decided once at begin:
the Wayland pinch protocol is `begin → update* → end`, so a gesture cannot be
taken over or handed back mid-flight.

While claimed, `Gesturing` owns the viewport: updates set the level directly
(`start_level × scale`, clamped to `1..=max-zoom`, snapped to 1), anchored on
the gesture-begin pointer position — or the viewport center while locked. No
follow runs during a gesture.

If the gesture loses its right to control the zoom mid-flight (session lock,
screenshot UI, MRU, pointer grab, Overview, owner output removal, or an
explicit zoom action taking over), the current gesture is committed and the
rest of the sequence is swallowed — the client never saw the begin.

`end` commits the displayed state; a cancelled gesture commits the same way.
After the gesture ends, a resting follow may begin if the pointer geometry
requires it.

## Overview

Zoom and Overview are never simultaneously fully-active presentation
transforms. The stored zoom state remains authoritative; suppression is
derived only at the presentation boundary (`effective_zoom_transform`): the
effective level moves between the stored level and 1 in `log2` space with the
overview progress, without touching stored state.

- Zoom actions during the Overview still apply to the stored state; the
  presentation stays suppressed.
- No hidden follow runs while the Overview is active (tracking gate).
- When the Overview closes, the current pointer geometry is re-evaluated and
  a new follow may begin.

## Session lock, screenshot UI, MRU

- **Session lock:** the lock surface is a replacement presentation drawn in
  raw screen space — shown unzoomed. The stored zoom state is preserved and
  restored on unlock. Pointer presentation uses the identity transform while
  locked; tracking is gated off, so no hidden follow accumulates.
- **Screenshot UI / MRU:** these compositor UIs suspend camera movement. An
  active follow is committed at its current displayed position; no stale
  timestamp survives the suspension.

## Debug overlay

`zoom.debug` draws screen-space diagnostics on the physical output only
(`RenderTarget::Output`): the red deadzone outline (a center crosshair when
`deadzone-size` is 0) and the amber focal marker (dimmed at 1×, where the
stored focal does not affect the identity transform). Hidden while the
Overview or the MRU UI is visually active; never part of screencasts or
screen captures.

The overlay is a passive reader of zoom state: it turns already-computed
geometry into elements and must never mutate the state.

## IPC

`niri msg zoom` / `Request::Zoom` returns one `ZoomState` per output:

```text
output           output name
level            currently displayed level (animation sample mid-transition)
target_level     user-requested level
effective_level  level after Overview suppression (1 while open)
focal            fixed point of the transform, output-local logical [x, y]
locked           zoom lock state
```

Transient transition internals are deliberately not exposed.

## Tests

Behavior is covered by unit tests in `src/layout/zoom.rs` and
`src/utils/view.rs`, and by integration tests in `src/tests/zoom.rs`
(render targets, geometry, damage, pointer/tablet/touch mapping, warps,
deadzone, follow, lock, actions, hold, pinch, config reload).
