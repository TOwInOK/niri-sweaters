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
- **[Zoom](#zoom).** Magnify with keyboard, mouse wheel, or touchpad pinch; follow the pointer, lock the view, or hold a key for temporary zoom. Transition smoothly into Overview.
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
            pattern "checker" // stockinette / rib / checker / zigzag / diamond / dots
            accent-color "#bac6a6" // second yarn colour
            stitch-size 8 // larger stitches mean fewer rows across the border
            relief 0.65 // depth and shading
            fuzz 0.15 // wool fluffiness
        }
    }
}
```

#### Per-window patterns and colours

```kdl
window-rule {
    match app-id=r#"^dev\.zed\.Zed$"#
    border {
        active-color "#526c89"
        inactive-color "#3d4652"
        knit {
            pattern "rib"
            accent-color "#8fa6bf"
            // ...
        }
    }
}
window-rule {
    match app-id=r#"^com\.mitchellh\.ghostty$"# title="^knit-desktop-helix$"
    border {
        active-color "#5a6b53"
        inactive-color "#3d4652"
        knit {
            pattern "checker"
            accent-color "#9ba98e"
            // ...
        }
    }
}
```

[Tuning guide](docs/wiki/Knitted-Border-Tuning.md) · [All knit options](docs/wiki/Configuration:-Layout.md#procedural-knit-border) · [Per-app rules](docs/wiki/Configuration:-Window-Rules.md#focus-ring-and-border)

### Zoom

Magnify the desktop without resizing windows. The view follows the pointer outside a central deadzone; lock it for a steady view, or hold a shortcut for a temporary close-up. Opening Overview smoothly ends the zoom session.

```kdl
zoom {
    max-zoom 100.0 // maximum zoom level
    increment-factor 1.2 // zoom step multiplier
    deadzone-size 0.2 // central area with no pointer-follow movement

    follow-min-speed 50 // follow speed just outside the deadzone
    follow-max-speed 6000 // follow speed far outside the deadzone

    sampling "auto" threshold=4.0 // linear below 4x; nearest at 4x and above (sharp pixels)
    // sampling "linear"
    // sampling "nearest"
    pinch-fingers 3

    // use to tune the deadzone and follow speeds
    debug {
        deadzone // red outline of the pointer-follow deadzone
        focal-point // fixed point of the zoom transform, not the deadzone boundary; dimmed at 1x
    }
}


binds {
    Mod+Z { zoom 2.0; } // toggle zoom
    Mod+X { zoom 2.0 hold=true; } // zoom while held; restore the previous view on release
    // Mod+X { zoom 2.0 hold=true lock=true; } // also lock pointer-follow while held

    Mod+Shift+Z { zoom-lock; } // toggle pointer-follow lock

    Mod+0 { reset-zoom; }
    Mod+WheelScrollUp { zoom-in; }
    Mod+WheelScrollDown { zoom-out; }
}
```

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
