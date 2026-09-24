<div align="center">
  <h1>niri-sweaters</h1>
  <p><strong>Your windows. In sweaters.</strong></p>
  <img src="docs/assets/knit-hero.webp" alt="Zed, Oh My Pi, Helix, and Obsidian windows with knitted borders" width="800">
  <p>A <a href="https://github.com/niri-wm/niri">niri</a> fork with knitted window borders and desktop zoom.<br>Choose patterns and colours for each app, magnify your desktop, and select regions for recording.</p>
  <p>
    <a href="#features">Features</a> ·
    <a href="#patterns">Patterns</a> ·
    <a href="#in-action">In action</a> ·
    <a href="#install">Install</a> ·
    <a href="#configure">Configure</a> ·
    <a href="#region-ipc">Region IPC</a>
  </p>
</div>

## Features

- **[Knitted borders](#knit).** Six patterns, per-app colours, and live config reloads; borders follow window geometry and animations.
- **[Desktop Zoom](#zoom).** Magnify with keyboard, mouse wheel, or touchpad pinch; follow the pointer, lock the view, or hold a key for temporary zoom. Transition smoothly into Overview.
- **[Region IPC](#region-ipc).** Select an area for its coordinates and display a recording frame on one monitor without including the frame in captures.
- **[Separate session](#install).** Run alongside stock Niri with its own config.

## Patterns

![Stockinette, rib, checker, zigzag, diamond, and dots](docs/assets/knit-patterns.webp)

## In action

[![A desktop with app-specific knitted borders](docs/assets/knit-desktop.png)](docs/assets/knit-desktop.png)

https://github.com/user-attachments/assets/7dcf712c-5059-4ca8-9076-512646c47815

[Example config](resources/knit-preview.kdl)

## Install

### Linux (systemd)

Install [Rust](https://rustup.rs/), Git, and the [build dependencies](docs/wiki/Getting-Started.md#building), then run:

```sh
git clone https://github.com/TOwInOK/niri-sweaters.git
cd niri-sweaters
./scripts/install-niri-sweaters
```

Log out and select **Niri Sweaters** in your display manager.

### NixOS

Add the input and module to your system flake:

```nix
{
  inputs.niri-sweaters.url = "github:TOwInOK/niri-sweaters";

  outputs = { nixpkgs, niri-sweaters, ... }: {
    nixosConfigurations.your-host = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ./configuration.nix
        niri-sweaters.nixosModules.default
      ];
    };
  };
}
```

```sh
sudo nixos-rebuild switch --flake .#your-host
```

Log out and select **Niri Sweaters**. Your existing niri config is copied to `~/.config/niri-sweaters/` on first login without overwriting existing files.

## Configure

Edit `~/.config/niri-sweaters/config.kdl`; changes reload live. Merge the examples into your existing sections.

### Knit

Give each window a knitted border. Choose `stockinette`, `rib`, `checker`, `zigzag`, `diamond`, or `dots`, then match the yarn colours to your apps.

```kdl
layout {
    focus-ring { off; }
    border {
        on
        width 24
        active-color "#465440"
        inactive-color "#384136"
        knit {
            on
            pattern "checker"
            accent-color "#bac6a6"
        }
    }
}
```

- **Border:** `width` sets its thickness; `active-color` and `inactive-color` set the base colours. `knit { off; }` restores an ordinary border.
- **Knit:** `accent-color` supplies the second yarn colour; `stitch-size` adjusts the stitch scale. Per-app window rules can override the pattern and palette.
- **Focus ring:** a separate focus indicator, disabled in this example so the knitted border stands on its own.

#### Per-window patterns and colours

Set your shared style in `layout { border { ... } }`, then add top-level `window-rule` blocks alongside `layout`, not inside it. Each rule overrides only the properties it specifies: the examples below keep the shared border width, stitch size, and enabled Knit state.

```kdl
window-rule {
    match app-id=r#"^dev\.zed\.Zed$"#
    border {
        active-color "#526c89"
        inactive-color "#526c89"
        knit {
            pattern "rib"
            accent-color "#8fa6bf"
        }
    }
}
window-rule {
    match app-id=r#"^com\.mitchellh\.ghostty$"# title="^knit-desktop-helix$"
    border {
        active-color "#5a6b53"
        inactive-color "#5a6b53"
        knit {
            pattern "checker"
            accent-color "#9ba98e"
        }
    }
}
```

- **Match an app:** the first rule gives Zed a blue rib pattern. Run `niri-sweaters msg pick-window` and click a window to find its app ID and title.
- **Match a specific window:** the second rule applies only to Ghostty windows titled `knit-desktop-helix`. Conditions on one `match` line must all match; separate `match` lines are alternatives. App IDs and titles use regular expressions; `^...$` matches the whole value.
- **Layer your rules:** matching rules apply in config order, with later values overriding earlier ones. Put general rules first and specific exceptions last. Set `border { knit { off; } }` in a rule to use a plain border for that window.
- **Mix palettes:** `active-color` and `inactive-color` colour the base yarn; `knit.accent-color` supplies the accent yarn. Border gradients work too—the [desktop example](resources/knit-desktop.kdl) uses them for Obsidian and a second Ghostty window.

[Tuning guide](docs/wiki/Knitted-Border-Tuning.md) · [All knit options](docs/wiki/Configuration:-Layout.md#procedural-knit-border) · [Per-app rules](docs/wiki/Configuration:-Window-Rules.md#focus-ring-and-border)

### Zoom

Magnify the desktop without resizing windows. The view follows the pointer outside a central deadzone; lock it for a steady view, or hold a shortcut for a temporary close-up. Opening Overview smoothly ends the zoom session.

```kdl
zoom {
    max-zoom 10.0
    increment-factor 1.2
    deadzone-size 0.5
    follow-min-speed 50
    follow-max-speed 6000
    sampling "auto" threshold=4.0
    pinch-fingers 3
}
binds {
    Mod+Z { zoom 2.0; }
    Mod+X { zoom 2.0 hold=true; }
    Mod+Shift+Z { zoom-lock; }
    Mod+0 { reset-zoom; }
    Mod+WheelScrollUp { zoom-in; }
    Mod+WheelScrollDown { zoom-out; }
}
```

- **Toggle or hold:** `Mod+Z` toggles 2× zoom; `Mod+X` restores the previous view when released. `Mod+Shift+Z` toggles pointer-follow lock; `Mod+0` resets zoom.
- **Wheel or pinch:** wheel-up multiplies zoom by 1.2 and wheel-down divides it by 1.2; three-finger pinch adjusts it continuously.
- **Tracking:** `deadzone-size 0.5` lets the pointer move within the central half of the display without panning.
- **Sampling:** `auto` keeps the image smooth below 4× and shows sharp pixel edges at 4× and above. Use `linear` for smooth filtering throughout or `nearest` for pixel inspection.

#### Tune pointer following

The example uses `follow-min-speed 50` for gentle movement just outside the deadzone and `follow-max-speed 6000` for fast travel near the screen edge. Both are measured in **displayed logical pixels per second**, not zoom increments. Speed rises smoothly as the pointer moves farther outside the deadzone and falls as the viewport catches up. The minimum must be positive; the maximum must be at least the minimum.

To see what you are tuning, temporarily add this inside `zoom`:

```kdl
    debug {
        deadzone
        focal-point
    }
```

- **Red outline:** the area where the pointer can move without panning. `deadzone-size 0.5` uses half the output width and height; smaller values start following sooner. At `0`, a red crosshair replaces the outline and tracking keeps the pointer centered; at `1`, following is effectively disabled.
- **Amber crosshair:** the fixed point of the zoom transform, not the deadzone boundary. It is dimmed at 1×.

Zoom in with pointer-follow unlocked, then tune one setting at a time:

1. Adjust `deadzone-size` until you can move comfortably inside the red outline without shifting the view.
2. Move just outside the outline. Lower `follow-min-speed` if the initial movement feels abrupt; raise it if following starts too slowly.
3. Move toward a screen edge. Lower `follow-max-speed` if travel is too fast; raise it if crossing the desktop takes too long. `6000` is a fast-travel choice, not a required default.

Following can continue after the mouse stops, until the pointer reaches the deadzone boundary or the viewport reaches the desktop edge. Remove `debug` when finished; changes reload live. Debug markers stay out of screenshots and recordings.

[Zoom controls](docs/wiki/Accessibility.md#desktop-zoom) · [All zoom settings](docs/wiki/Configuration:-Miscellaneous.md#zoom) · [Key bindings](docs/wiki/Configuration:-Key-Bindings.md#zoom)

### Region IPC

Select an area for a recorder, then mark it with a coloured frame. Unlike window borders, this frame stays fixed on one monitor across zoom, Overview, and workspace switches. It does not receive input or appear in screenshots and screencasts.

Drag with the left mouse button and release to select; **Escape** cancels. JSON returns the output name and coordinates in logical pixels relative to that output. `--format slurp` instead returns global logical coordinates as `x,y widthxheight`.

- **Screenshots while zoomed:** enable zoom lock on the capture monitor **before selecting** and keep it locked until the frame is captured. Selection pauses pointer-follow only while dragging; after release, the view can move before the screenshot is taken. Check `niri-sweaters msg --json zoom` first: `niri-sweaters msg action zoom-lock` toggles the lock on the output under the pointer, so do not toggle an already locked view. After capture, cancellation, or an error, restore the previous lock state—unlock only if your script enabled the lock.
- **Video or coordinates only:** zoom lock is not required. Leave pointer-follow available so you can pan the zoomed desktop while recording; the selected region itself stays fixed.

This example uses `jq` to put a frame around the exact selection:

```sh
region=$(niri-sweaters msg --json select-region) || exit 1
printf '%s\n' "$region" | jq -c '{RegionFrame:{Set:{region:.,color:"yellow"}}}' | niri-sweaters msg raw-request
niri-sweaters msg region-frame set-color red
niri-sweaters msg --json region-frame get
niri-sweaters msg region-frame set-color orange
niri-sweaters msg region-frame clear
```

A recording script can use **yellow → red → orange** for countdown, recording, and WebP conversion, then clear the frame when finished. Niri provides the selection and frame; a recorder such as `wf-recorder` captures the area, and `ffmpeg` converts it to animated WebP. Convert logical coordinates to physical pixels using the output scale when required by the recorder.

[IPC command reference](docs/wiki/IPC.md#region-selection-and-recording-frame)

## License

Based on [niri](https://github.com/niri-wm/niri) by Ivan Molodetskikh and contributors. Inspired by [window-sweaters](https://github.com/saragordic/window-sweaters). [GPL-3.0-or-later](LICENSE).
