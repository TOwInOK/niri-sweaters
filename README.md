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

- **Knitted borders.** Six patterns, per-app colours, and live config reloads; borders follow window geometry and animations.
- **Desktop Zoom.** Magnify with keyboard, mouse wheel, or touchpad pinch; follow the pointer, lock the view, or hold a key for temporary zoom. Transition smoothly into Overview.
- **Region IPC.** Select an area for its coordinates and display a recording frame on one monitor without including the frame in captures.
- **Separate session.** Run alongside stock Niri with its own config.

## Patterns

![Stockinette, rib, checker, zigzag, diamond, and dots](docs/assets/knit-patterns.webp)

## In action

[![A desktop with app-specific knitted borders](docs/assets/knit-desktop.png)](docs/assets/knit-desktop.png)

<video controls width="800" src="docs/assets/zoom_demonstration.mp4"></video>

[Watch the Desktop Zoom demo (MP4)](docs/assets/zoom_demonstration.mp4) · [Example config](resources/knit-preview.kdl)

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

Add this to the `layout` section of `~/.config/niri-sweaters/config.kdl`:

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

[Tuning guide](docs/wiki/Knitted-Border-Tuning.md) · [All knit options](docs/wiki/Configuration:-Layout.md#procedural-knit-border) · [Per-app rules](docs/wiki/Configuration:-Window-Rules.md#focus-ring-and-border)

## Region IPC

```sh
niri-sweaters msg --json select-region
niri-sweaters msg region-frame set --output DP-1 --x 100 --y 80 --width 800 --height 600 --color red
niri-sweaters msg region-frame clear
```

[IPC command reference](docs/wiki/IPC.md#region-selection-and-recording-frame)

## License

Based on [niri](https://github.com/niri-wm/niri) by Ivan Molodetskikh and contributors. Inspired by [window-sweaters](https://github.com/saragordic/window-sweaters). [GPL-3.0-or-later](LICENSE).
