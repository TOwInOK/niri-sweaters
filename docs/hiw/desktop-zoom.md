# How Desktop Zoom Works

Developer guide for contributors and coding agents. This describes the current implementation, not the original proposal. Read it before changing zoom ownership, input mapping, or Overview integration; update it alongside changes to those contracts.

User controls are documented in [Accessibility](../wiki/Accessibility.md) and [key bindings](../wiki/Configuration:-Key-Bindings.md#zoom).

## Source map and ownership

Paths below are relative to the repository root.

| Area | Source | Responsibility |
| --- | --- | --- |
| Zoom domain | `src/layout/zoom.rs` | Per-output FSM, commands, follow physics, restore and gesture viewport ownership |
| Output integration | `src/layout/monitor.rs` | Owns `OutputZoomState`, Overview handoff payload and output geometry |
| Overview lifecycle | `src/layout/mod.rs` | Captures/rebases handoffs before replacing Overview progress; workspace and drag geometry |
| Input | `src/input/mod.rs` | Actions, hold sessions, pinch protocol routing, Overview entry and pointer rebasing |
| Presentation | `src/niri.rs` | Separate scene/pointer transforms, hit testing, rendering and per-frame tracking |
| Geometry primitive | `src/utils/view.rs` | `ViewportTransform` and inverse mapping |
| Grabs | `src/input/{move_grab,spatial_movement_grab,touch_overview_grab}.rs` | Convert pointer positions and deltas to scene coordinates |
| Diagnostics | `src/ui/zoom_debug.rs` | Passive screen-space overlay |
| Public interfaces | `niri-config`, `niri-ipc`, `src/ipc/server.rs` | Configuration/actions and derived IPC state |
| Regression coverage | `src/tests/zoom.rs` | Rendered frames, input protocols and lifecycle integration |

Desktop Zoom magnifies the rendered desktop, not client DPI. Clients keep their scale; magnification does not request higher-resolution client buffers. Use output/UI scaling for permanent UI sizing.

## Zoom FSM

`Monitor` owns one `OutputZoomState` per output. It is an enum with **eight** variants, not a struct of independent flags:

| Variant | Payload / owner |
| --- | --- |
| `Idle(ZoomView)` | Resting unlocked view; may be above 1× |
| `Follow { view, follow }` | `FollowState` moves the focal at a resting level |
| `Zooming { view, zooming }` | `ZoomingState::Command` or `ZoomingState::Restore` |
| `ZoomingFollow { view, zooming, follow }` | `ZoomCommandState` plus `CombinedFollow` |
| `Gesture { view, gesture }` | `GesturePayload` directly owns the level |
| `Locked(ZoomView)` | Resting locked view |
| `LockedZooming { view, zooming }` | `LockedZoomingState::Command(LockedCommandState)` |
| `LockedGesture { view, gesture }` | `LockedGesturePayload` directly owns the level |

`ZoomView` contains committed `level`, `focal`, and `view_size`. Payload fields are private; callers use domain operations rather than constructing or mutating payloads.

The types exclude locked follow, restore/gesture plus follow, and multiple concurrent zoom operations. Gestures are separate top-level variants, not members of `ZoomingState`. A locked restore becomes a locked command; there is no locked restore payload.

### Commands and derived values

Both `ZoomCommandState` and `LockedCommandState` have `In`, `Out`, `Value`, and `ToIdentity` variants. The first three own a target resolved when the command is created. `ToIdentity` has no numeric target field: its target is exactly 1.

- `level()` and `focal()` sample the current presentation, not merely committed fields.
- `target_level()` derives the target from the active operation; at rest it equals the committed level, during restore it is the destination, and during gesture it is the latest gesture level.
- `intent_level()` delegates to `target_level()`. Repeated incremental actions use intent, not an intermediate animation sample.
- `is_locked()` derives from the locked variants. `is_animating()` describes clock-driven work; gestures drive their own redraws and are not clock animations.

There is no standalone mutable target, lock flag, or optional transition on `OutputZoomState`.

A new command samples the displayed view and replaces the old operation rather than queuing another one. Level commands animate in `log2(level)`; springs can preserve velocity, while easing retargets preserve position. A resting no-op need not allocate an animation. Disabling animation commits the destination immediately.

### Completion and identity

- Follow completion becomes `Idle`.
- Combined follow completing first leaves `Zooming(Command)`.
- A combined command completing first can become resting `Follow`, constructed from live geometry.
- Both completing leaves `Idle`; identity completion always leaves `Idle` at exact 1×.
- Locked command completion leaves `Locked`.
- Gesture end commits the view to `Idle` or `Locked`; subsequent tracking may start a fresh follow.
- `end_session()` replaces any variant with unlocked `Idle` at 1× and a clamped sampled focal.

The anchored focal solve divides by `level - 1`. `ToIdentity` therefore uses a fixed focal payload instead of solving an anchor near 1×. Restore-to-identity likewise keeps its source focal until completion. Numeric guards and snapping remain necessary for geometry, but numeric equality does not replace lifecycle variants.

The viewport must remain inside the output: focal coordinates are clamped to the output bounds. `update_view_size()` commits active resting follow geometry, updates size, and clamps the committed focal; at identity it resets that focal to the output center.

## Coordinate and input domains

For an output-local point, the basic transform is:

`display = focal + (content - focal) * factor`

`ViewportTransform` supplies forward/inverse point and rectangle mapping. At factor 1 it is identity regardless of focal. Global coordinates additionally include the output origin.

Do not assume that the canonical pointer and the scene always share a coordinate space:

- During ordinary Zoom, pointer and scene presentation use the same Zoom transform. Canonical pointer positions are content coordinates.
- During Overview, including its handoff tail, pointer presentation is identity. Canonical pointer positions are screen-space positions, while the scene can still have a non-identity correction.
- During session lock, pointer presentation is identity because the lock surface is drawn unzoomed.

The input boundary expresses this explicitly:

```text
P = pointer_presentation_transform
S = scene_presentation_transform
scene_position_within_output(p) = S.apply_inverse(P.apply(p))
scene_delta_scale = P.factor() / S.factor()
```

Under ordinary Zoom the position composition is identity. During handoff it applies the inverse residual. Never inverse-transform an already mapped scene point again.

Relative device motion is adjusted by pointer presentation scale. Absolute pointer and output-mapped tablet input use inverse pointer presentation; scene hit testing subsequently applies the scene mapping. Touch follows the relevant display-to-scene mapping without starting pointer follow. Focused-window tablet mapping targets logical window geometry directly, not a second inverse Zoom transform.

Programmatic warps use canonical coordinates. Locked Zoom clamps them to its presented viewport; unlocked warps can start deadzone follow. Hot corners and screen-space UI must not receive the scene inverse.

### Drag positions across camera frames

Move, spatial-movement and touch-overview grabs convert positions and deltas at the input boundary. Layout stores interactive-move and DnD pointer positions after applying the handoff transform, and applies the **current** inverse handoff when consuming them. With no handoff these conversions are identity.

This keeps the stored position independent of the moving camera. Caching an inverse-mapped scene point instead makes a grabbed window or insertion target drift under a stationary pointer. Rendering, hit testing, insertion and DnD scrolling must all use the current mapping.

## Deadzone follow

The centered deadzone has per-axis size `deadzone-size * output_size`. Zero gives a center point; one covers the output. Outside it, follow advances every frame, even without physical pointer motion.

Physics uses displayed geometry: overshoot is the vector from the nearest deadzone point to the pointer. Normalize each axis by the available distance between that deadzone border and the corresponding output edge. The maximum normalized depth sets intensity; the normalized overshoot vector sets direction independently, avoiding a diagonal speed boost.

Speed varies between `follow-min-speed` and `follow-max-speed` (defaults 80 and 1400 displayed logical pixels/second), currently using smoothstep. Each step is capped by the remaining reachable distance. Viewport bounds take precedence: if the clamp prevents further movement, follow finishes even if the pointer remains outside the deadzone.

Resting `FollowState` moves the focal; `CombinedFollow` moves `FlexibleAnchor.display` during a command. They share physics, not lifecycle payloads. Each owns its timing; suspension discards follow timing so resumed tracking starts without stale elapsed time.

`zoom_tracking_enabled` gates tracking during session lock, Overview presentation, screenshot UI and MRU. `Niri::update_zoom_follow` runs per frame for the pointer-owning output and commits resting follows on other outputs.

## Lock, hold and pinch

### Lock anchoring

Autonomous locked commands preserve the content at the viewport center. Locking samples the current presentation, drops combined follow, and changes anchor policy without a jump. Locking during Restore abandons its focal destination and converts it to a center-anchored command.

Unlocking an autonomous command re-anchors it on the **canonical pointer**, using its currently displayed position. It does not restore an old follow. `ToIdentity` retains its fixed-focal special case.

Gesture lock semantics are different: a gesture begun locked has a center anchor; locking mid-gesture freezes the current focal. Unlocking that gesture preserves its current focal mode. Do not apply autonomous command re-anchoring rules to an ongoing pinch.

### Hold session and Restore

Hold sessions live outside the viewport FSM. Press records the owning output and a `ZoomSnapshot` of displayed level, derived target and focal, then issues the requested command. Release targets that output even if the pointer has moved elsewhere.

Unlocked Restore animates progress 0→1: level interpolates in log space towards the saved **target**, focal towards the saved focal. At destination 1×, the source focal stays fixed until completion. A snapshot's sampled level is not the resting restore destination.

Restore excludes follow, but explicit pointer interaction or another zoom action can take over, replacing Restore with a command and abandoning its focal destination. Frame-only follow evaluation does not itself take over Restore.

While locked, animated restoration becomes a normal locked command to the saved level: it preserves viewport-center anchoring, not a constant focal for all levels. With animations off, immediate restore restores the saved target and clamped focal while preserving the current lock state.

Re-entry does not nest sessions: same-output presses retain the original snapshot; another output ends the old hold first. Lost-release cleanup restores immediately. Overview entry is a different boundary: it terminates the hold without restoring the old Zoom session.

### Pinch protocol ownership

Opt-in `pinch-fingers` is matched once at begin. Claim gates include session lock, screenshot/MRU UI, pointer grabs and Overview ownership. The external routing lifecycle is absent/idle, `ZoomPinchRouting::Active`, or swallowing the remainder; it is separate from `Gesture`/`LockedGesture` viewport states.

Updates set `start_level * scale`, clamped to configured limits and snapped near 1×. Direction can reverse within one sequence. End, including cancellation, commits the current view. An interrupted claimed sequence is swallowed through its end: clients never received its begin, so updates must not be forwarded as an orphan gesture.

## Overview owns the camera after entry

Overview does not suppress a live Zoom session and later restore it. The handoff is:

1. Layout captures each output's **displayed** scale and focal before replacing Overview progress; command targets are not snapshots of the displayed frame.
2. Overview owns a geometric `OverviewHandoff` payload on each affected monitor.
3. Input entry terminates Zoom, hold ownership and claimed pinch ownership. `end_session()` leaves Zoom in unlocked `Idle` at exact 1×.
4. Pointer/tablet canonical positions are rebased to their previous displayed positions. This preserves the visible hotspot; it is not a physical move to the screen center.
5. Overview renders the captured geometry through its transition. Exit returns to the ordinary desktop, never the old Zoom session.

Repeated open does not restart entry cleanup while already open. Reversal samples the live handoff rather than recapturing a Zoom target. New outputs and removed outputs must not inherit another output's camera payload.

### Centered scale and translation

For an ordinary opening, let `u` be Overview progress, `K` the configured Overview scale, `L` the captured Zoom level, `F` its desktop focal reference, and `C` the output center:

`B(u) = 1 + (K - 1) * u` — normal Overview scale.

`S(u) = L + (K - L) * u` — desired total scale.

The outer correction has factor `R = S / B` and focal:

`F_residual = C + B * (F - C)`

This is the calculation in `Monitor::overview_handoff_transform()`. For the centered active-workspace geometry, composing it with normal Overview gives translation:

`T(u) = (1 - u) * (1 - L) * F + u * (1 - K) * C`

Scale and translation therefore share progress: the existing frame immediately shrinks and moves towards the centered Overview geometry. The mouse is not moved to the center. Keeping the outer focal fixed at `F` instead introduces a different translation trajectory and an unwanted lateral camera flight.

These equations describe the centered scale/translation component, not a replacement for workspace switching, physical-pixel rounding or the existing workspace layout. Progress follows the configured Overview easing/spring; it is not necessarily linear in time. Crossing total scale 1 is allowed and does not imply a reset to an ordinary desktop frame. The residual representation avoids solving a total focal through division by `S - 1` at that crossing.

### Handoff driver lifecycle

`OverviewHandoff` stores the desktop focal reference, captured stacking order and an `OverviewHandoffDriver`:

- `Segment`: captures start/end progress and total scales; derives normalized segment progress from the live Overview progress. Reversal rebases from the current displayed scale. Spring overshoot is extrapolated rather than freezing the camera at the endpoint.
- `Gesture`: piecewise scale interpolation towards open Overview above the starting progress and towards desktop below it; the closed-end branch supports rubberband motion.
- `Animation`: its own normalized 0→1 driver using Overview animation configuration when start and end progress coincide. A zero-motion cancelled gesture still needs to animate the captured camera back to desktop.

Total scale has a positive lower bound of 0.0001. The no-Zoom path creates no unnecessary handoff. Payloads are retired when their driver completes, not reconstructed every frame.

A cancellation tail can outlive `overview_progress`. Consequently:

- `overview_active()` includes both Overview progress and a surviving handoff payload.
- Zoom actions, follow and pinch claims remain gated until camera ownership actually ends; otherwise a hidden Zoom can start behind the tail.
- Rendering selects the non-identity handoff correction independently of the Overview open intent flag.
- The handoff captures fullscreen/top-layer ordering to preserve the initial frame, then releases that override at completion.

## Rendering and replacement UI

`render_inner` applies `scene_presentation_transform` through the existing `push_desktop!` rescale wrapper. Normal desktop Zoom uses its viewport transform; Overview uses its outer correction in addition to the base workspace layout. Identity skips the wrapper.

Output, screencast and screen-capture targets use consistent desktop geometry. Pointer sprites remain unscaled; their position uses the separate pointer transform. Screen-space UI and hot corners must not receive the scene transform.

Session lock renders an unzoomed replacement surface and preserves Zoom state for unlock. Screenshot and MRU UI suspend tracking rather than accumulate hidden follow time. These are not Overview's terminate-session semantics.

The debug overlay is a passive reader: deadzone outline and focal marker, screen-space, output target only. It is hidden during Overview ownership and MRU, and excluded from captures. It must never mutate Zoom state.

## Actions and IPC

- `zoom-in` / `zoom-out` multiply/divide derived intent by `increment-factor`, clamp to `[1, max-zoom]`, and snap near identity using `ZOOM_SNAP_TO_ONE_EPSILON`.
- `set-zoom-level` validates the requested level; `reset-zoom` requests identity.
- `toggle-zoom` uses intent, not the sampled animation level. Its `hold=true` option also locks while zoomed and unlocks on toggling back.
- `zoom-lock hold=true` temporarily inverts lock state; release restores it. `hold-zoom` restores the previous view, with optional temporary locking.
- Hold actions requiring a physical release are bind-only, not IPC actions. Wheel triggers cannot provide that lifecycle.
- The target is the output under the canonical pointer, falling back to the active output. Hold release uses its session's owning output.
- Reducing `max-zoom` on config reload clamps existing Zoom state immediately, including locked outputs.

`niri msg zoom` returns per-output `ZoomState` through `src/ipc/server.rs`:

| Field | Current source / meaning |
| --- | --- |
| `output` | Output name |
| `level` | `zoom.level()`: current Zoom presentation sample |
| `target_level` | `zoom.target_level()`: derived intent |
| `effective_level` | Currently also `zoom.level()` |
| `focal` | `zoom.focal()` in output-local logical coordinates |
| `locked` | `zoom.is_locked()` |

After Overview entry, these Zoom levels are 1 and lock is false, even while the visible scene is still magnified by the handoff. **IPC does not expose the total Overview camera scale or residual transform.** Do not infer rendered scene geometry from `effective_level` alone. The IPC schema remains separate from internal FSM payloads.

## Verification and change checklist

Start with the owning code in the source map, then inspect callers. Modify Zoom through domain operations; keep hold/pinch protocol ownership outside the viewport FSM. Keep Overview geometry outside Zoom after session termination.

Relevant existing coverage:

- Model tests in `src/layout/zoom.rs`: enum transitions, derived targets, repeated actions, command replacement, completion order, identity, lock re-anchoring and Restore takeover.
- Geometry tests in `src/utils/view.rs`: forward/inverse transforms.
- Integration tests in `src/tests/zoom.rs`: rendered output, capture targets, pointer/tablet/touch mapping, holds, pinch routing, output isolation, config reload and Overview.
- `zoom_overview_handoff_*`: initial-frame preservation (including fullscreen and a live Zoom animation), simultaneous scale/translation, reversal, zero-motion cancellation and ownership tail, multiple outputs, hit testing, drag anchoring and capture consistency.

Run from the repository root:

~~~sh
cargo test --lib zoom_overview_handoff
cargo test --lib zoom
cargo test --lib
cargo test -p niri-config
cargo test -p niri-ipc
cargo check --workspace
cargo test --all --exclude niri-visual-tests
cargo +nightly fmt --all -- --check
~~~

These are verification commands, not a claim that every visual configuration has been covered. For presentation changes, also build and launch an isolated nested compositor from an existing Wayland session, using a dedicated config and its own IPC socket. Exercise zoomed entry, early reversal, repeated open, pointer motion and drag; compare intermediate frames, not only endpoints. Do not restart the user's main compositor or update golden images merely to hide a discrepancy.

Before completing a change, check:

1. The first rendered frame is continuous, including pointer hotspot and fullscreen stacking.
2. Scale **and translation** follow the intended path; a scalar-only test cannot catch sideways camera flight.
3. Input uses the same scene geometry as rendering, without transforming screen-space UI or applying an inverse twice.
4. Stationary grabs remain anchored while the camera changes.
5. Restore and gesture never gain a concurrent follow; repeated commands keep target intent.
6. Overview terminates the previous Zoom session and owns the full tail. No hidden Zoom starts during cancellation.
7. Reversal, identity crossing, no-motion gestures, output changes and animation-off paths remain finite and continuous where applicable.
8. Public config/IPC changes are intentional, and this guide and user documentation reflect the resulting contract.

Avoid reviving obsolete designs: an optional transition plus independent lock/target fields; a live Zoom session suppressed beneath Overview; fixed residual focal throughout entry; or a literal cursor warp to the center as a substitute for camera geometry.
