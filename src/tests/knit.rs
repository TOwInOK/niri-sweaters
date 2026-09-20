use std::ffi::CStr;
use std::path::PathBuf;

use niri_config::Config;
use smithay::backend::renderer::gles::ffi;
use smithay::output::Output;
use smithay::reexports::gbm::Format as Fourcc;
use smithay::utils::{Physical, Scale, Size, Transform};

use super::client::ClientId;
use super::*;
use crate::render_helpers::{render_to_vec, RenderCtx, RenderTarget};

/// The GL_RENDERER string of the primary renderer.
fn renderer_name(state: &mut crate::niri::State) -> String {
    state
        .backend
        .headless()
        .with_primary_renderer(|renderer| {
            renderer
                .with_context(|gl| unsafe {
                    let ptr = gl.GetString(ffi::RENDERER);
                    if ptr.is_null() {
                        return String::new();
                    }
                    CStr::from_ptr(ptr as *const _)
                        .to_string_lossy()
                        .into_owned()
                })
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

/// The golden images are only deterministic on llvmpipe, so the tests require it.
fn assert_llvmpipe(state: &mut crate::niri::State) {
    let name = renderer_name(state);
    assert!(
        name.to_lowercase().contains("llvmpipe"),
        "golden tests require the llvmpipe software renderer, got: {name}\n\
         force Mesa EGL with: \
         __EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json cargo test knit"
    );
}

/// Renders an output to a physical-size RGBA pixel buffer.
fn render_output_rgba(
    state: &mut crate::niri::State,
    output: &Output,
) -> (Size<i32, Physical>, Vec<u8>) {
    state.niri.update_render_elements(Some(output));

    let size = output.current_mode().unwrap().size;
    let scale = Scale::from(output.current_scale().fractional_scale());

    let pixels = state
        .backend
        .headless()
        .with_primary_renderer(|renderer| {
            let ctx = RenderCtx {
                renderer,
                target: RenderTarget::Output,
                xray: None,
            };
            let elements = state.niri.render_to_vec(ctx, output, false);
            render_to_vec(
                renderer,
                size,
                scale,
                Transform::Normal,
                Fourcc::Abgr8888,
                elements.iter().rev(),
            )
            .expect("error rendering output")
        })
        .expect("no primary renderer");

    (size, pixels)
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/tests/golden")
        .join(format!("{name}.png"))
}

/// Compares the rendered pixels against the golden image.
///
/// Run with `NIRI_GOLDEN_UPDATE=1` to (re)generate the golden images.
fn assert_golden(name: &str, size: Size<i32, Physical>, pixels: &[u8]) {
    let path = golden_path(name);
    let width = size.w as u32;
    let height = size.h as u32;

    if std::env::var_os("NIRI_GOLDEN_UPDATE").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = std::fs::File::create(&path).unwrap();
        crate::utils::write_png_rgba8(file, width, height, pixels).unwrap();
        eprintln!("wrote golden {}", path.display());
        return;
    }

    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(err) => panic!(
            "error opening golden {}: {err}\n\
             generate it with: NIRI_GOLDEN_UPDATE=1 cargo test knit",
            path.display()
        ),
    };

    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = decoder.read_info().unwrap();
    let mut golden = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut golden).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    assert_eq!(
        (info.width, info.height),
        (width, height),
        "golden {name} size mismatch"
    );

    let differing = golden
        .chunks_exact(4)
        .zip(pixels.chunks_exact(4))
        .filter(|(a, b)| a.iter().zip(b.iter()).any(|(a, b)| a.abs_diff(*b) > 2))
        .count();
    let total = usize::try_from(width).unwrap() * usize::try_from(height).unwrap();
    let percent = differing as f64 / total as f64 * 100.;

    if percent > 1. {
        let new_path = path.with_file_name(format!("{name}.new.png"));
        if let Ok(file) = std::fs::File::create(&new_path) {
            let _ = crate::utils::write_png_rgba8(file, width, height, pixels);
        }
        panic!(
            "golden {name} mismatch: {differing} of {total} pixels differ ({percent:.2}%), \
             wrote {}",
            new_path.display()
        );
    }
}

fn set_up(config_text: &str) -> Fixture {
    let config = Config::parse_mem(config_text).unwrap();
    let mut f = Fixture::with_config(config);
    f.niri_state().backend.headless().add_renderer().unwrap();
    f.add_output(1, (1920, 720));
    f
}

fn open_window(f: &mut Fixture, id: ClientId, title: &str, w: u16, h: u16, rgba: [u32; 4]) {
    let window = f.client(id).create_window();
    let surface = window.surface.clone();
    window.set_title(title);
    window.commit();
    f.double_roundtrip(id);

    let window = f.client(id).window(&surface);
    // Match the viewport destination to the size niri configured, so the window
    // fills its tile exactly and the border hugs it with no gap.
    let (cw, ch) = window
        .configures_received
        .last()
        .map(|(_, c)| c.size)
        .unwrap_or((0, 0));
    let w = u16::try_from(cw).unwrap_or(w);
    let h = u16::try_from(ch).unwrap_or(h);
    window.attach_new_buffer_with_color(rgba[0], rgba[1], rgba[2], rgba[3]);
    window.set_size(w, h);
    window.ack_last_and_commit();
    f.double_roundtrip(id);
}

const KNIT_PATTERNS_CONFIG: &str = r##"
animations {
    off
}

hotkey-overlay {
    skip-at-startup
}

layout {
    gaps 8
    default-column-width { fixed 220; }
    focus-ring {
        off
    }
    border {
        on
        width 48
        active-color "#526c89"
        inactive-color "#526c89"
        knit {
            on
            stitch-size 8
            relief 0.65
            fuzz 0.15
        }
    }
}

window-rule {
    match title="knit-stockinette"
    border {
        knit {
            pattern "stockinette"
            accent-color "#8fa6bf"
        }
    }
}

window-rule {
    match title="knit-rib"
    border {
        knit {
            pattern "rib"
            accent-color "#8fa6bf"
        }
    }
}

window-rule {
    match title="knit-checker"
    border {
        knit {
            pattern "checker"
            accent-color "#8fa6bf"
        }
    }
}

window-rule {
    match title="knit-zigzag"
    border {
        knit {
            pattern "zigzag"
            accent-color "#8fa6bf"
        }
    }
}

window-rule {
    match title="knit-diamond"
    border {
        knit {
            pattern "diamond"
            accent-color "#8fa6bf"
        }
    }
}

window-rule {
    match title="knit-dots"
    border {
        knit {
            pattern "dots"
            accent-color "#8fa6bf"
        }
    }
}

window-rule {
    match title="knit-off"
    draw-border-with-background false
    border {
        knit {
            off
        }
    }
}
"##;

const KNIT_ROUNDED_CONFIG: &str = r##"
animations {
    off
}

hotkey-overlay {
    skip-at-startup
}

layout {
    gaps 8
    default-column-width { fixed 220; }
    focus-ring {
        off
    }
    border {
        on
        width 30
        active-color "#526c89"
        inactive-color "#526c89"
        knit {
            on
            pattern "checker"
            accent-color "#8fa6bf"
            stitch-size 8
            relief 0.65
            fuzz 0.15
        }
    }
}

window-rule {
    match title="knit-r24"
    geometry-corner-radius 24
}

window-rule {
    match title="knit-r6"
    geometry-corner-radius 6
}
"##;

const KNIT_FRACTIONAL_CONFIG: &str = r##"
animations {
    off
}

hotkey-overlay {
    skip-at-startup
}

output "headless-1" {
    scale 1.5
}

layout {
    gaps 8
    default-column-width { fixed 220; }
    focus-ring {
        off
    }
    border {
        on
        width 30
        active-color "#526c89"
        inactive-color "#526c89"
        knit {
            on
            pattern "checker"
            accent-color "#8fa6bf"
            stitch-size 8
            relief 0.65
            fuzz 0.15
        }
    }
}

window-rule {
    match title="knit-r24"
    geometry-corner-radius 24
}
"##;

#[test]
fn knit_patterns() {
    let mut f = set_up(KNIT_PATTERNS_CONFIG);
    assert_llvmpipe(f.niri_state());

    let id = f.add_client();

    let windows = [
        ("knit-stockinette", 0x1a1a1a1a),
        ("knit-rib", 0x1c1a1a1a),
        ("knit-checker", 0x1a1c1a1a),
        ("knit-zigzag", 0x1a1a1c1a),
        ("knit-diamond", 0x1c1c1a1a),
        ("knit-dots", 0x1a1c1c1a),
        ("knit-off", 0x1a1a1a1a),
    ];
    for (title, shade) in windows {
        open_window(
            &mut f,
            id,
            title,
            124,
            560,
            [shade, shade, shade, 0xffffffff],
        );
    }

    let output = f.niri_output(1);
    let (size, pixels) = render_output_rgba(f.niri_state(), &output);
    assert_golden("knit_patterns", size, &pixels);
}

#[test]
fn knit_rounded_corners() {
    let mut f = set_up(KNIT_ROUNDED_CONFIG);
    assert_llvmpipe(f.niri_state());

    let id = f.add_client();

    open_window(&mut f, id, "knit-r24", 400, 560, [0x1a1a1a1a; 4]);
    open_window(&mut f, id, "knit-r6", 400, 560, [0x1a1a1a1a; 4]);

    let output = f.niri_output(1);
    let (size, pixels) = render_output_rgba(f.niri_state(), &output);
    assert_golden("knit_rounded_corners", size, &pixels);
}

#[test]
fn knit_fractional_scale() {
    let mut f = set_up(KNIT_FRACTIONAL_CONFIG);
    assert_llvmpipe(f.niri_state());

    let id = f.add_client();

    open_window(&mut f, id, "knit-r24", 400, 560, [0x1a1a1a1a; 4]);

    let output = f.niri_output(1);
    let (size, pixels) = render_output_rgba(f.niri_state(), &output);
    assert_golden("knit_fractional_scale", size, &pixels);
}
