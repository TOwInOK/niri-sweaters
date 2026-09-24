You can communicate with the running niri instance over an IPC socket.
Check `niri msg --help` for available commands.

The `--json` flag prints the response in JSON, rather than formatted.
For example, `niri msg --json outputs`.

> [!TIP]
> If you're getting parsing errors from `niri msg` after upgrading niri, make sure that you've restarted niri itself.
> You might be trying to run a newer `niri msg` against an older `niri` compositor.

### Event Stream

<sup>Since: 0.1.9</sup>

While most niri IPC requests return a single response, the event stream request will make niri continuously stream events into the IPC connection until it is closed.
This is useful for implementing various bars and indicators that update as soon as something happens, without continuous polling.

The event stream IPC is designed to give you the complete current state up-front, then follow up with updates to that state.
This way, your state can never "desync" from niri, and you don't need to make any other IPC information requests.

Where reasonable, event stream state updates are atomic, though this is not always the case.
For example, a window may end up with a workspace id for a workspace that had already been removed.
This can happen if the corresponding workspaces-changed event arrives before the corresponding window-changed event.

To get a taste of the events, run `niri msg event-stream`.
Though, this is more of a debug function than anything.
You can get raw events from `niri msg --json event-stream`, or by connecting to the niri socket and requesting an event stream manually.

You can find the full list of events along with documentation [here](https://niri-wm.github.io/niri/niri_ipc/enum.Event.html).

### Programmatic Access

`niri msg --json` is a thin wrapper over writing and reading to a socket.
When implementing more complex scripts and modules, you're encouraged to access the socket directly.

Connect to the UNIX domain socket located at `$NIRI_SOCKET` in the filesystem.
Write your request encoded in JSON on a single line, followed by a newline character, or by flushing and shutting down the write end of the connection.
Read the reply as JSON, also on a single line.

You can use `socat` to test communicating with niri directly:

```sh
$ socat STDIO "$NIRI_SOCKET"
"FocusedWindow"
{"Ok":{"FocusedWindow":{"id":12,"title":"t socat STDIO /run/u ~","app_id":"Alacritty","workspace_id":6,"is_focused":true}}}
```

The reply is an `Ok` or an `Err` wrapping the same JSON object as you get from `niri msg --json`.

<sup>Since: next release</sup>
For more complex requests, you can pass `--print-request` to `niri msg` to see how the request should be formatted:

```sh
$ niri msg --print-request action focus-workspace 2
{"Action":{"FocusWorkspace":{"reference":{"Index":2}}}}
```

This is the format that you should use for communicating with the niri socket directly.
You can also use `niri msg raw-request` to send a raw JSON request without `socat`:

```sh
$ echo '{"Action":{"FocusWorkspace":{"reference":{"Id":8}}}}' | niri msg raw-request
"Handled"
```

You can find all available requests and response types in the [niri-ipc sub-crate documentation](https://niri-wm.github.io/niri/niri_ipc/).

### Desktop Zoom

<sup>Since: next release</sup>

`niri msg zoom` prints the [desktop zoom](./Accessibility.md#desktop-zoom) state of each output.
With `--json`, the response is a list of objects with the following fields:

- `output`: the output name.
- `level`: the currently displayed zoom level. While a zoom transition is in progress this is the current animation sample; during a pinch gesture it is the level set by the latest gesture update.
- `target_level`: the zoom level the output is transitioning towards. It equals `level` at rest and during a pinch gesture, and differs while a zoom animation or a `zoom hold=true` restore is in progress.
- `effective_level`: currently the same as `level`.
- `focal`: the fixed point of the current zoom transform in output-local logical coordinates, as `[x, y]`.
- `locked`: whether the zoom lock is enabled.

Opening the [Overview](./Overview.md) ends the zoom session, so while it is open `level`, `target_level` and `effective_level` report `1` and `locked` reports `false`—even though the Overview transition still renders the captured zoomed frame.
IPC does not expose the Overview's own camera transform, so the rendered scene scale cannot be derived from `effective_level`.

### Region Selection and Recording Frame

`niri msg select-region` selects a rectangle on one monitor without taking a screenshot
or changing the clipboard. Press the left mouse button, drag, and release to confirm;
press Escape to cancel. An empty click lets you try again. Selection cannot cross
monitor boundaries. Desktop zoom pointer-follow is suspended during selection.

For a capture that should match the view at selection time, it can be useful
to enable [zoom lock](./Accessibility.md#desktop-zoom) before `select-region`
and keep it until the first frame is captured. Selection suspends pointer-follow
only while it is open: after release the zoomed scene can move under the fixed
selection before the recorder gets a frame. `select-region` does not lock zoom
itself. Preserve an existing zoom lock rather than toggling it off, and restore
the previous state if your script enabled the lock. Leave zoom unlocked if the
recording is supposed to show the viewport moving under the selected region.

```sh
niri msg select-region                       # x y width height
niri msg select-region --format slurp        # x,y widthxheight
niri msg --json select-region                # output + output-local geometry
```

Plain and slurp formats use global logical coordinates, including negative output
positions. JSON returns, for example:

```json
{"output":"DP-1","geometry":{"x":100,"y":80,"width":800,"height":600}}
```

JSON geometry is output-local and measured in logical pixels, not physical video
pixels. Convert coordinates according to the recording tool and output scale.
Cancellation exits unsuccessfully: plain/slurp print nothing to stdout, JSON prints
`null`, and diagnostics go to stderr. `--format json` also selects JSON; `--json`
conflicts with explicitly selecting plain or slurp.

The separate recording frame marks a fixed region of one monitor:

```sh
niri msg region-frame set --output DP-1 --x 100 --y 80 --width 800 --height 600 --color red
niri msg region-frame set-color orange
niri msg region-frame get
niri msg --json region-frame get
niri msg region-frame clear
```

There is one frame, with no frame ID. Set replaces it, set-color changes only its
color, get returns its specification (or `null` with JSON when absent), and clear
is idempotent. Colors use the same CSS syntax as configuration colors. An invalid
update leaves the current frame unchanged. Set-color fails if no frame exists.

The inward border is two logical pixels wide. It keeps its output-local position
and size across desktop zoom, Overview, and workspace switches; it follows its
monitor when outputs are rearranged and never appears on another monitor. It is
hidden while locked, excluded from screenshots and screencasts, and does not receive
input. Output resizing clips the border without changing its stored geometry.
Disconnecting its monitor removes the frame. It otherwise remains until cleared
or niri exits, independently of selection sessions and IPC client connections.

A recording script can consume the JSON selection and install the same frame:

```sh
region=$(niri msg --json select-region) || exit 1
printf '%s\n' "$region" | jq -c '{RegionFrame:{Set:{region:.,color:"red"}}}' | niri msg raw-request
```

Keep recording lifecycle in the script, and call `region-frame clear` from its
exit cleanup (for example, an `EXIT` trap). Niri does not launch or control the recorder.

The socket protocol always uses JSON. `"SelectRegion"` waits for one response:
`{"Ok":{"SelectedRegion":{"region":{...},"global_geometry":{...}}}}`, or
`{"Ok":{"SelectedRegion":null}}` on cancellation. Both coordinate representations
are captured together; the CLI JSON form prints only `region`. Frame requests are
`{"RegionFrame":{"Set":{...}}}`, `{"RegionFrame":{"SetColor":"red"}}`,
`{"RegionFrame":"Get"}`, and `{"RegionFrame":"Clear"}`. Get returns
`{"Ok":{"RegionFrame":{"region":{...},"color":"red"}}}` or a null payload;
mutations return `{"Ok":"Handled"}`.

Selecting while the session is locked, with no outputs, or during another
interactive grab/UI returns an error. Losing the selected output or changing
its logical size cancels selection. Disconnecting the requesting IPC client
does not automatically cancel the interactive selection; finish it or press Escape.

### Backwards Compatibility

The JSON output *should* remain stable, as in:

- existing fields and enum variants should not be renamed
- non-optional existing fields should not be removed

However, new fields and enum variants will be added, so you should handle unknown fields or variants gracefully where reasonable.

The formatted/human-readable output (i.e. without `--json` flag) is **not** considered stable.
Please prefer the JSON output for scripts, since I reserve the right to make any changes to the human-readable output.

The `niri-ipc` sub-crate (like other niri sub-crates) is *not* API-stable in terms of the Rust semver; rather, it follows the version of niri itself.
In particular, new struct fields and enum variants will be added.
