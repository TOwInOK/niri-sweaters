//! Offscreen documentation-figure renderer.
//!
//! Renders the Knitted-Border-Tuning documentation PNGs and the two README
//! WebP assets through the real niri GLES knit shader path — no Wayland
//! session, clients or compositor — reads the framebuffer back and writes the
//! canonical assets under docs/. One headless renderer is created per
//! invocation and reused for every render job.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context as _};
use niri::backend::Headless;
use niri::layout::focus_ring::{FocusRing, FocusRingRenderElement};
use niri::render_helpers::border::BorderRenderElement;
use niri::render_helpers::solid_color::SolidColorRenderElement;
use niri::render_helpers::texture::{TextureBuffer, TextureRenderElement};
use niri::render_helpers::{render_to_texture, render_to_vec};
use niri::utils::{to_physical_precise_round, write_png_rgba8};
use niri_config::{Color, CornerRadius, Gradient, GradientInterpolation, KnitBorder, KnitPattern};
use pango::{Style, Weight};
use pangocairo::cairo::{self, ImageSurface};
use pangocairo::pango::FontDescription;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::{Id, Kind};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::Color32F;
use smithay::utils::{Logical, Physical, Point, Rectangle, Scale, Size, Transform};
use webp_rust::{ImageBuffer, LosslessEncodingConfig};

/// Render target pixel format.
const TARGET_FORMAT: Fourcc = Fourcc::Abgr8888;
/// Render elements produced by one `FocusRing` in border mode.
const ELEMENTS_PER_RING: usize = 8;

/// Canvas background of every figure: the shared `#eee7d8` backdrop used by
/// the README assets and resources/*.kdl `background-color`.
const CANVAS_RGB: [u8; 3] = [0xee, 0xe7, 0xd8];
/// Per-cell interior fill (and the whole tuning-start canvas).
const CELL_RGB: [u8; 3] = CANVAS_RGB;
/// Label text color, sampled from the references.
const TEXT_RGB: [u8; 3] = [0x20, 0x25, 0x2a];

/// One palette row of the palettes figure: active and inactive yarn colors
/// as `#rrggbb` strings, matching the table in Knitted-Border-Tuning.md.
struct Palette {
    name: &'static str,
    active_base: &'static str,
    active_accent: &'static str,
    inactive_base: &'static str,
    inactive_accent: &'static str,
}

const PALETTES: [Palette; 4] = [
    Palette {
        name: "Slate",
        active_base: "#526c89",
        active_accent: "#8fa6bf",
        inactive_base: "#3d4652",
        inactive_accent: "#566273",
    },
    Palette {
        name: "Sage",
        active_base: "#5a6b53",
        active_accent: "#9ba98e",
        inactive_base: "#3e473a",
        inactive_accent: "#596651",
    },
    Palette {
        name: "Plum",
        active_base: "#6c5878",
        active_accent: "#aa97b7",
        inactive_base: "#463d4b",
        inactive_accent: "#64576c",
    },
    Palette {
        name: "Oat",
        active_base: "#796952",
        active_accent: "#b7a78b",
        inactive_base: "#494239",
        inactive_accent: "#685e51",
    },
];

/// One column of the size-patterns figure: a pattern with its fixed palette.
/// The first four reuse the documented palettes; Diamond and Dots have no
/// documented colors, so their values are estimated from the reference bitmap.
struct PatternColumn {
    name: &'static str,
    pattern: KnitPattern,
    base: &'static str,
    accent: &'static str,
}

const PATTERN_COLUMNS: [PatternColumn; 6] = [
    PatternColumn {
        name: "Stockinette",
        pattern: KnitPattern::Stockinette,
        base: "#526c89",
        accent: "#8fa6bf",
    },
    PatternColumn {
        name: "Rib",
        pattern: KnitPattern::Rib,
        base: "#5a6b53",
        accent: "#9ba98e",
    },
    PatternColumn {
        name: "Checker",
        pattern: KnitPattern::Checker,
        base: "#6c5878",
        accent: "#aa97b7",
    },
    PatternColumn {
        name: "Zigzag",
        pattern: KnitPattern::Zigzag,
        base: "#796952",
        accent: "#b7a78b",
    },
    PatternColumn {
        name: "Diamond",
        pattern: KnitPattern::Diamond,
        base: "#7a5a50",
        accent: "#b08d7e",
    },
    PatternColumn {
        name: "Dots",
        pattern: KnitPattern::Dots,
        base: "#45726d",
        accent: "#7a9a90",
    },
];

/// One row of the size-patterns figure: border width, inner radius and
/// stitch size from the ready-made sizes table in Knitted-Border-Tuning.md.
struct SizeRow {
    name: &'static str,
    label: &'static str,
    border_width: f64,
    inner_radius: f64,
    stitch_size: f64,
}

const SIZE_ROWS: [SizeRow; 5] = [
    SizeRow {
        name: "Thin edging",
        label: "B 6\nR 12\nS 3.2",
        border_width: 6.,
        inner_radius: 12.,
        stitch_size: 3.2,
    },
    SizeRow {
        name: "Thin, more visible knit",
        label: "B 8\nR 12\nS 4.25",
        border_width: 8.,
        inner_radius: 12.,
        stitch_size: 4.25,
    },
    SizeRow {
        name: "Compact",
        label: "B 19\nR 16\nS 5",
        border_width: 19.,
        inner_radius: 16.,
        stitch_size: 5.,
    },
    SizeRow {
        name: "Balanced",
        label: "B 30\nR 24\nS 8",
        border_width: 30.,
        inner_radius: 24.,
        stitch_size: 8.,
    },
    SizeRow {
        name: "Large motif",
        label: "B 45\nR 30\nS 8",
        border_width: 45.,
        inner_radius: 30.,
        stitch_size: 8.,
    },
];

// All element kinds a figure can contain: solid fills, rounded fills, knit
// ring segments and text labels. `render_to_vec` needs a single concrete
// element type.
smithay::backend::renderer::element::render_elements! {
    FigureElement<=GlesRenderer>;
    Fill = SolidColorRenderElement,
    RoundedFill = BorderRenderElement,
    Ring = FocusRingRenderElement,
    Label = TextureRenderElement<GlesTexture>,
}

/// Parameters of one knit-border render job. Everything the job needs is in
/// this struct; jobs never mutate shared state, so job order cannot affect
/// the output.
struct RenderSpec {
    /// Job context for error messages, e.g.
    /// "size-patterns row=Balanced pattern=Diamond".
    context: String,
    /// Window contents size in logical pixels. The window is intentionally
    /// larger than the visible frame so only the top edge, left edge and
    /// top-left corner of the ring are visible, like in the reference assets.
    win_size: Size<f64, Logical>,
    /// Window contents origin; the ring extends `border_width` outward.
    location: Point<f64, Logical>,
    border_width: f64,
    /// Outer corner radius of the ring: inner radius + border width.
    outer_radius: f32,
    pattern: KnitPattern,
    base: Color,
    accent: Color,
    stitch_size: f64,
    /// Optional gradient replacing the flat base color, matching the
    /// `active-gradient`/`inactive-gradient` border options.
    gradient: Option<Gradient>,
    relief: f64,
    fuzz: f64,
    is_active: bool,
    scale: f64,
    alpha: f32,
}

/// Owns the headless backend and the single GLES renderer shared by every
/// render job in this invocation.
struct VisualGenerator {
    headless: Headless,
}

impl VisualGenerator {
    fn new() -> anyhow::Result<Self> {
        let mut headless = Headless::new();
        // Initializes resources and shaders internally; do not init them again.
        headless
            .add_renderer()
            .context("error creating headless renderer")?;
        Ok(Self { headless })
    }

    fn generate(&mut self, wiki_dir: &Path, assets_dir: &Path) -> anyhow::Result<()> {
        self.headless
            .with_primary_renderer(|renderer| {
                compose_tuning_start(renderer, &wiki_dir.join("knit-tuning-start.png"))?;
                compose_size_patterns(renderer, &wiki_dir.join("knit-tuning-size-patterns.png"))?;
                compose_palettes(renderer, &wiki_dir.join("knit-tuning-palettes.png"))?;
                compose_hero(renderer, &assets_dir.join("knit-hero.webp"))?;
                compose_patterns(renderer, &assets_dir.join("knit-patterns.webp"))?;
                Ok(())
            })
            .context("no primary renderer")?
    }
}

fn hex(s: &str) -> Color {
    let n = u32::from_str_radix(s.trim_start_matches('#'), 16).expect("hex color");
    Color::from_rgba8_unpremul(
        ((n >> 16) & 0xff) as u8,
        ((n >> 8) & 0xff) as u8,
        (n & 0xff) as u8,
        0xff,
    )
}

/// Builds the job `FocusRing` and collects its render elements. Every
/// element must use the shader-backed `Gradient` variant; a fallback to
/// `SolidColorRenderElement` would mean the knit shader path is not in use.
fn prepare_elements(
    renderer: &mut GlesRenderer,
    spec: &RenderSpec,
) -> anyhow::Result<Vec<FocusRingRenderElement>> {
    ensure!(
        BorderRenderElement::has_shader(renderer),
        "border shader is not available"
    );

    let mut ring = FocusRing::new(niri_config::FocusRing {
        off: false,
        width: spec.border_width,
        active_color: spec.base,
        inactive_color: spec.base,
        urgent_color: spec.base,
        active_gradient: spec.gradient,
        inactive_gradient: spec.gradient,
        urgent_gradient: None,
    });
    ring.update_knit(Some(KnitBorder {
        off: false,
        pattern: spec.pattern,
        accent_color: spec.accent,
        stitch_size: spec.stitch_size,
        relief: spec.relief,
        fuzz: spec.fuzz,
    }));

    // Only used for workspace-relative gradients, which are disabled here.
    let view_rect = Rectangle::new(
        Point::from((-spec.border_width, -spec.border_width)),
        spec.win_size + Size::from((spec.border_width * 2., spec.border_width * 2.)),
    );

    ring.update_render_elements(
        spec.win_size,
        spec.is_active,
        true,
        false,
        view_rect,
        CornerRadius::from(spec.outer_radius),
        spec.scale,
        spec.alpha,
    );

    let mut elements = Vec::new();
    ring.render(renderer, spec.location, &mut |elem| elements.push(elem));

    ensure!(
        elements.len() == ELEMENTS_PER_RING,
        "expected {ELEMENTS_PER_RING} render elements, got {}",
        elements.len()
    );
    ensure!(
        elements
            .iter()
            .all(|elem| matches!(elem, FocusRingRenderElement::Gradient(_))),
        "some elements fell back to SolidColorRenderElement"
    );

    Ok(elements)
}

/// Renders one knit-border job into figure elements.
fn ring_elements(
    renderer: &mut GlesRenderer,
    spec: &RenderSpec,
) -> anyhow::Result<Vec<FigureElement>> {
    let elements = prepare_elements(renderer, spec)
        .with_context(|| format!("failed to render {}", spec.context))?;
    Ok(elements.into_iter().map(FigureElement::from).collect())
}

/// Renders one matrix cell into its own texture. Rendering to a separate
/// target hard-crops the ring at the cell edges, matching the reference
/// composition where only the top edge, left edge and top-left corner of a
/// larger window are visible.
fn cell_element(
    renderer: &mut GlesRenderer,
    spec: &RenderSpec,
    cell: Size<f64, Logical>,
    location: Point<f64, Logical>,
) -> anyhow::Result<FigureElement> {
    let mut elements = vec![fill(Point::from((0., 0.)), cell, CELL_RGB)];
    elements.extend(ring_elements(renderer, spec)?);

    let size = Size::<i32, Physical>::from((
        (cell.w * spec.scale).round() as i32,
        (cell.h * spec.scale).round() as i32,
    ));
    let (texture, _sync) = render_to_texture(
        renderer,
        size,
        Scale::from(spec.scale),
        Transform::Normal,
        TARGET_FORMAT,
        elements.into_iter(),
    )
    .with_context(|| format!("error rendering cell {}", spec.context))?;

    let buffer =
        TextureBuffer::from_texture(renderer, texture, spec.scale, Transform::Normal, Vec::new());
    Ok(TextureRenderElement::from_texture_buffer(
        buffer,
        location,
        1.,
        None,
        None,
        Kind::Unspecified,
    )
    .into())
}

/// Opaque solid-color rectangle: canvas background or per-cell interior.
fn fill(loc: Point<f64, Logical>, size: Size<f64, Logical>, rgb: [u8; 3]) -> FigureElement {
    let [r, g, b] = rgb;
    let color = Color32F::new(r as f32 / 255., g as f32 / 255., b as f32 / 255., 1.);
    SolidColorRenderElement::new(
        Id::new(),
        Rectangle::new(loc, size),
        CommitCounter::default(),
        color,
        Kind::Unspecified,
    )
    .into()
}

/// Opaque rounded rectangle used for window bodies and card interiors. The
/// border shader with `border_width = 0` fills the whole rounded geometry.
/// `overlap` expands the rect (and its radius) under the ring's inner
/// anti-aliased edge so the two AA bands cannot both go translucent and let
/// the background show through as a seam.
fn rounded_fill(
    loc: Point<f64, Logical>,
    size: Size<f64, Logical>,
    radius: CornerRadius,
    overlap: f64,
    rgb: [u8; 3],
) -> FigureElement {
    let [r, g, b] = rgb;
    let color = Color::from_rgba8_unpremul(r, g, b, 0xff);
    let size = size + Size::from((overlap * 2., overlap * 2.));
    let radius = CornerRadius {
        top_left: radius.top_left + overlap as f32,
        top_right: radius.top_right + overlap as f32,
        bottom_right: radius.bottom_right + overlap as f32,
        bottom_left: radius.bottom_left + overlap as f32,
    };
    let geometry = Rectangle::new(Point::from((0., 0.)), size);
    BorderRenderElement::new(
        size,
        geometry,
        GradientInterpolation::default(),
        color,
        color,
        0.,
        geometry,
        0.,
        radius,
        1.,
        1.,
    )
    .with_location(loc - Size::from((overlap, overlap)))
    .into()
}

/// Text style for `measure_label`/`label_element`: pango font description
/// parameters plus the rendered color.
struct TextStyle {
    family: &'static str,
    size_px: f64,
    weight: Weight,
    style: Style,
    rgb: [u8; 3],
}

impl TextStyle {
    fn font(&self, scale: f64) -> FontDescription {
        let mut font = FontDescription::new();
        font.set_family(self.family);
        font.set_weight(self.weight);
        font.set_style(self.style);
        font.set_size((self.size_px * f64::from(pango::SCALE)) as i32);
        font.set_absolute_size(to_physical_precise_round(scale, font.size()));
        font
    }
}

/// The default documentation label style.
fn label_style(size_px: f64) -> TextStyle {
    TextStyle {
        family: "sans",
        size_px,
        weight: Weight::Normal,
        style: Style::Normal,
        rgb: TEXT_RGB,
    }
}

/// Measures a label without uploading it: pango layout on a dummy surface.
fn measure_label(text: &str, style: &TextStyle, scale: f64) -> anyhow::Result<Size<f64, Logical>> {
    let font = style.font(scale);

    let surface = ImageSurface::create(cairo::Format::ARgb32, 0, 0)?;
    let cr = cairo::Context::new(&surface)?;
    let layout = pangocairo::functions::create_layout(&cr);
    layout.context().set_round_glyph_positions(false);
    layout.set_font_description(Some(&font));
    layout.set_text(text);
    let (width, height) = layout.pixel_size();
    ensure!(width > 0 && height > 0, "label rendered empty: {text:?}");

    Ok(Size::from((
        f64::from(width) / scale,
        f64::from(height) / scale,
    )))
}

/// Renders a text label into a GLES texture through the same pango/cairo
/// stack niri uses for its UI, wrapped in a texture render element.
fn label_element(
    renderer: &mut GlesRenderer,
    text: &str,
    style: &TextStyle,
    scale: f64,
    location: Point<f64, Logical>,
) -> anyhow::Result<FigureElement> {
    let font = style.font(scale);

    let surface = ImageSurface::create(cairo::Format::ARgb32, 0, 0)?;
    let cr = cairo::Context::new(&surface)?;
    let layout = pangocairo::functions::create_layout(&cr);
    layout.context().set_round_glyph_positions(false);
    layout.set_font_description(Some(&font));
    layout.set_text(text);
    let (width, height) = layout.pixel_size();
    ensure!(width > 0 && height > 0, "label rendered empty: {text:?}");

    let surface = ImageSurface::create(cairo::Format::ARgb32, width, height)?;
    let cr = cairo::Context::new(&surface)?;
    let [r, g, b] = style.rgb;
    cr.set_source_rgb(
        f64::from(r) / 255.,
        f64::from(g) / 255.,
        f64::from(b) / 255.,
    );
    pangocairo::functions::show_layout(&cr, &layout);
    drop(cr);
    let data = surface.take_data().unwrap();

    let buffer = TextureBuffer::from_memory(
        renderer,
        &data,
        Fourcc::Argb8888,
        (width, height),
        false,
        scale,
        Transform::Normal,
        Vec::new(),
    )?;

    Ok(TextureRenderElement::from_texture_buffer(
        buffer,
        location,
        1.,
        None,
        None,
        Kind::Unspecified,
    )
    .into())
}

/// Renders all elements into one target and returns the checked RGBA
/// readback plus its pixel dimensions.
fn render_canvas(
    renderer: &mut GlesRenderer,
    name: &str,
    canvas: Size<f64, Logical>,
    scale: f64,
    elements: Vec<FigureElement>,
) -> anyhow::Result<(u32, u32, Vec<u8>)> {
    let size = Size::<i32, Physical>::from((
        (canvas.w * scale).round() as i32,
        (canvas.h * scale).round() as i32,
    ));
    let pixels = render_to_vec(
        renderer,
        size,
        Scale::from(scale),
        Transform::Normal,
        TARGET_FORMAT,
        elements.into_iter(),
    )
    .with_context(|| format!("error rendering {name}"))?;

    let (width, height) = (size.w as u32, size.h as u32);
    ensure!(width > 0, "{name}: target width must be greater than 0");
    ensure!(height > 0, "{name}: target height must be greater than 0");
    ensure!(
        pixels.len() == (width * height * 4) as usize,
        "{name}: unexpected RGBA buffer length: {} (expected {})",
        pixels.len(),
        width * height * 4
    );
    ensure!(
        pixels.as_chunks::<4>().0.iter().any(|px| px[3] != 0),
        "{name}: rendered image is fully transparent"
    );

    Ok((width, height, pixels))
}

/// Renders all elements into one target, checks the readback and writes the
/// PNG. The PNG goes to a temp file next to the target first so a failed
/// encode can never leave a partial file in place of a good asset.
fn render_figure(
    renderer: &mut GlesRenderer,
    name: &str,
    canvas: Size<f64, Logical>,
    scale: f64,
    elements: Vec<FigureElement>,
    output: &Path,
) -> anyhow::Result<()> {
    let (width, height, pixels) = render_canvas(renderer, name, canvas, scale, elements)?;
    write_png_atomic(output, width, height, &pixels)
}

/// Writes an RGBA buffer as PNG through a temp file next to the destination,
/// renamed into place only after a fully successful encode and write.
fn write_png_atomic(output: &Path, width: u32, height: u32, pixels: &[u8]) -> anyhow::Result<()> {
    if let Some(dir) = output.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("error creating output dir {}", dir.display()))?;
    }
    let tmp = output.with_extension("tmp");
    {
        let file =
            File::create(&tmp).with_context(|| format!("error creating {}", tmp.display()))?;
        write_png_rgba8(BufWriter::new(file), width, height, pixels)
            .with_context(|| format!("error encoding {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, output)
        .with_context(|| format!("error renaming {} to {}", tmp.display(), output.display()))?;

    println!("saved {}", output.display());
    Ok(())
}

/// Renders all elements into one target and writes the lossless WebP used by
/// the README assets, through the same temp-file-then-rename discipline as
/// the PNG path.
fn render_figure_webp(
    renderer: &mut GlesRenderer,
    name: &str,
    canvas: Size<f64, Logical>,
    scale: f64,
    elements: Vec<FigureElement>,
    output: &Path,
) -> anyhow::Result<()> {
    let (width, height, pixels) = render_canvas(renderer, name, canvas, scale, elements)?;
    write_webp_atomic(output, width, height, &pixels)
        .with_context(|| format!("failed to encode {}", output.display()))
}

/// Encodes an RGBA buffer as lossless WebP and atomically replaces the
/// destination. Lossless keeps the knit stitch detail crisp; the default
/// `z_level` is fixed in code so output stays deterministic.
fn write_webp_atomic(output: &Path, width: u32, height: u32, rgba: &[u8]) -> anyhow::Result<()> {
    let image = ImageBuffer {
        width: width as usize,
        height: height as usize,
        rgba: rgba.to_vec(),
    };
    let data =
        webp_rust::encode_lossless_with_config(&image, &LosslessEncodingConfig::default(), None)
            .context("error encoding WebP")?;

    if let Some(dir) = output.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("error creating output dir {}", dir.display()))?;
    }
    let tmp = output.with_extension("tmp");
    std::fs::write(&tmp, &data).with_context(|| format!("error writing {}", tmp.display()))?;
    std::fs::rename(&tmp, output)
        .with_context(|| format!("error renaming {} to {}", tmp.display(), output.display()))?;

    println!("saved {}", output.display());
    Ok(())
}

/// The starter figure: one checker border in the Slate palette, same
/// geometry as the verified PoC — a 200x78 logical canvas fully covered by
/// the window, so only the top edge, left edge and top-left corner show.
fn compose_tuning_start(renderer: &mut GlesRenderer, output: &Path) -> anyhow::Result<()> {
    let canvas = Size::from((200., 78.));
    let scale = 8.;

    let spec = RenderSpec {
        context: "tuning-start".to_owned(),
        win_size: Size::from((200., 80.)),
        location: Point::from((30., 30.)),
        border_width: 30.,
        // Inner radius 24 + border width 30.
        outer_radius: 54.,
        pattern: KnitPattern::Checker,
        base: hex(PALETTES[0].active_base),
        accent: hex(PALETTES[0].active_accent),
        stitch_size: 8.,
        gradient: None,
        relief: 0.65,
        fuzz: 0.15,
        is_active: true,
        scale,
        alpha: 1.,
    };

    let mut elements = vec![fill(Point::from((0., 0.)), canvas, CELL_RGB)];
    elements.extend(ring_elements(renderer, &spec)?);

    render_figure(
        renderer,
        "knit-tuning-start",
        canvas,
        scale,
        elements,
        output,
    )
}

/// The size x pattern matrix: 5 ready-made size rows x 6 pattern columns.
/// Each cell is a 112x88 logical crop of the same top-left-corner
/// composition as tuning-start; one palette per column.
fn compose_size_patterns(renderer: &mut GlesRenderer, output: &Path) -> anyhow::Result<()> {
    let canvas = Size::from((800., 548.));
    let cell = Size::from((112., 88.));
    let origin = Point::from((72., 44.));
    let pitch = Size::<f64, Logical>::from((120., 100.));
    let scale = 5.;
    let style = label_style(10.);

    let mut elements = vec![fill(Point::from((0., 0.)), canvas, CANVAS_RGB)];

    // Knit cells, each rendered into its own texture for a hard crop.
    for (row_i, row) in SIZE_ROWS.iter().enumerate() {
        for (col_i, col) in PATTERN_COLUMNS.iter().enumerate() {
            let cell_loc = origin + Size::from((pitch.w * col_i as f64, pitch.h * row_i as f64));
            let spec = RenderSpec {
                context: format!("size-patterns row={} pattern={}", row.name, col.name),
                win_size: Size::from((cell.w - row.border_width, cell.h - row.border_width)),
                location: Point::from((row.border_width, row.border_width)),
                border_width: row.border_width,
                outer_radius: (row.inner_radius + row.border_width) as f32,
                pattern: col.pattern,
                base: hex(col.base),
                accent: hex(col.accent),
                stitch_size: row.stitch_size,
                gradient: None,
                relief: 0.65,
                fuzz: 0.15,
                is_active: true,
                scale,
                alpha: 1.,
            };
            elements.push(cell_element(renderer, &spec, cell, cell_loc)?);
        }
    }

    // Column headers, centered over each cell.
    for (col_i, col) in PATTERN_COLUMNS.iter().enumerate() {
        let size = measure_label(col.name, &style, scale)
            .with_context(|| format!("error measuring label {}", col.name))?;
        let loc = Point::from((
            origin.x + pitch.w * col_i as f64 + (cell.w - size.w) / 2.,
            origin.y - 19.2 - size.h,
        ));
        elements.push(
            label_element(renderer, col.name, &style, scale, loc)
                .with_context(|| format!("error rendering label {}", col.name))?,
        );
    }

    // Row labels, top-aligned with each cell row.
    for (row_i, row) in SIZE_ROWS.iter().enumerate() {
        let loc = Point::from((12., origin.y + pitch.h * row_i as f64));
        elements.push(
            label_element(renderer, row.label, &style, scale, loc)
                .with_context(|| format!("error rendering label {}", row.name))?,
        );
    }

    render_figure(
        renderer,
        "knit-tuning-size-patterns",
        canvas,
        scale,
        elements,
        output,
    )
}

/// The palette matrix: 4 palettes x active/inactive, each cell the same
/// 200x78 logical composition as tuning-start (checker, R 24, B 30, S 8).
fn compose_palettes(renderer: &mut GlesRenderer, output: &Path) -> anyhow::Result<()> {
    let canvas = Size::from((450., 400.));
    let cell = Size::from((200., 78.));
    let origin = Point::from((10., 17.));
    let pitch = Size::<f64, Logical>::from((225., 100.));
    let scale = 8.;
    let style = label_style(7.);

    let mut elements = vec![fill(Point::from((0., 0.)), canvas, CANVAS_RGB)];

    // Knit cells: active on the left, inactive on the right, each rendered
    // into its own texture for a hard crop.
    for (row_i, palette) in PALETTES.iter().enumerate() {
        for (col_i, is_active) in [true, false].into_iter().enumerate() {
            let (base, accent) = if is_active {
                (palette.active_base, palette.active_accent)
            } else {
                (palette.inactive_base, palette.inactive_accent)
            };
            let cell_loc = origin + Size::from((pitch.w * col_i as f64, pitch.h * row_i as f64));
            let spec = RenderSpec {
                context: format!(
                    "palettes palette={} state={}",
                    palette.name,
                    if is_active { "active" } else { "inactive" }
                ),
                win_size: Size::from((cell.w - 30., cell.h - 30.)),
                location: Point::from((30., 30.)),
                border_width: 30.,
                outer_radius: 54.,
                pattern: KnitPattern::Checker,
                base: hex(base),
                accent: hex(accent),
                stitch_size: 8.,
                relief: 0.65,
                gradient: None,
                fuzz: 0.15,
                is_active,
                scale,
                alpha: 1.,
            };
            elements.push(cell_element(renderer, &spec, cell, cell_loc)?);
        }
    }

    // Cell labels above each cell: "Slate active: #526c89 / #8fa6bf".
    for (row_i, palette) in PALETTES.iter().enumerate() {
        for (col_i, is_active) in [true, false].into_iter().enumerate() {
            let (base, accent, state) = if is_active {
                (palette.active_base, palette.active_accent, "active")
            } else {
                (palette.inactive_base, palette.inactive_accent, "inactive")
            };
            let text = format!("{} {}: {} / {}", palette.name, state, base, accent);
            let size = measure_label(&text, &style, scale)
                .with_context(|| format!("error measuring label {text:?}"))?;
            let cell_loc = origin + Size::from((pitch.w * col_i as f64, pitch.h * row_i as f64));
            let loc = Point::from((cell_loc.x + 0.6, cell_loc.y - 4.4 - size.h));
            elements.push(
                label_element(renderer, &text, &style, scale, loc)
                    .with_context(|| format!("error rendering label {text:?}"))?,
            );
        }
    }

    render_figure(
        renderer,
        "knit-tuning-palettes",
        canvas,
        scale,
        elements,
        output,
    )
}

/// One window of the hero cascade: interior geometry, fill color and the
/// knit border spec. Window order in `HERO_WINDOWS` is back to front.
struct HeroWindow {
    name: &'static str,
    /// Window contents origin; the ring extends `HERO_BORDER` outward.
    loc: Point<f64, Logical>,
    size: Size<f64, Logical>,
    interior: [u8; 3],
    pattern: KnitPattern,
    base: &'static str,
    accent: &'static str,
    gradient: Option<Gradient>,
}

/// The README hero: four overlapping windows cascading top-left to
/// bottom-right, each with a real knit border. Geometry is explicit, sampled
/// from the reference asset; palettes and patterns match
/// resources/knit-desktop.kdl (Zed, Helix, Oh My Pi, Obsidian).
fn compose_hero(renderer: &mut GlesRenderer, output: &Path) -> anyhow::Result<()> {
    let canvas = Size::from((1280., 640.));
    const BORDER: f64 = 24.;
    const INNER_RADIUS: f32 = 24.;
    const SCALE: f64 = 1.;

    let gradient = |from: &'static str, to: &'static str, angle: i16| Gradient {
        from: hex(from),
        to: hex(to),
        angle,
        relative_to: niri_config::GradientRelativeTo::Window,
        in_: GradientInterpolation::default(),
    };

    let windows = [
        HeroWindow {
            name: "zed",
            loc: Point::from((54., 30.)),
            size: Size::from((901., 700.)),
            interior: [0x2e, 0x34, 0x3e],
            pattern: KnitPattern::Rib,
            base: "#526c89",
            accent: "#8fa6bf",
            gradient: None,
        },
        HeroWindow {
            name: "helix",
            loc: Point::from((122., 101.)),
            size: Size::from((951., 700.)),
            interior: [0x17, 0x1a, 0x16],
            pattern: KnitPattern::Checker,
            base: "#5a6b53",
            accent: "#9ba98e",
            gradient: None,
        },
        HeroWindow {
            name: "oh-my-pi",
            loc: Point::from((231., 172.)),
            size: Size::from((1006., 700.)),
            interior: [0x26, 0x27, 0x35],
            pattern: KnitPattern::Stockinette,
            base: "#945ba1",
            accent: "#f5e2b8",
            gradient: Some(gradient("#945ba1", "#477f97", 110)),
        },
        HeroWindow {
            name: "obsidian",
            loc: Point::from((316., 246.)),
            size: Size::from((1000., 700.)),
            interior: [0x1c, 0x1c, 0x1c],
            pattern: KnitPattern::Diamond,
            base: "#6c5878",
            accent: "#aa97b7",
            gradient: Some(gradient("#6c5878", "#aa97b7", 45)),
        },
    ];

    let mut elements = vec![fill(Point::from((0., 0.)), canvas, CANVAS_RGB)];

    for (i, win) in windows.iter().enumerate() {
        let spec = RenderSpec {
            context: format!("hero window {} ({})", i + 1, win.name),
            win_size: win.size,
            location: win.loc,
            border_width: BORDER,
            outer_radius: INNER_RADIUS + BORDER as f32,
            pattern: win.pattern,
            base: hex(win.base),
            accent: hex(win.accent),
            gradient: win.gradient,
            stitch_size: 8.,
            relief: 0.65,
            fuzz: 0.15,
            is_active: true,
            scale: SCALE,
            alpha: 1.,
        };
        elements.push(rounded_fill(
            win.loc,
            win.size,
            CornerRadius::from(INNER_RADIUS),
            0.,
            win.interior,
        ));
        elements.extend(
            ring_elements(renderer, &spec)
                .with_context(|| format!("failed to render hero window {}", i + 1))?,
        );

        if win.name == "obsidian" {
            hero_obsidian_text(renderer, &mut elements, win.loc)?;
        }
    }

    render_figure_webp(renderer, "knit-hero", canvas, SCALE, elements, output)
}

/// The foreground window's text block: title, body lines and the italic
/// sign-off, plus the purple caret before the first body line.
fn hero_obsidian_text(
    renderer: &mut GlesRenderer,
    elements: &mut Vec<FigureElement>,
    interior: Point<f64, Logical>,
) -> anyhow::Result<()> {
    let title = TextStyle {
        family: "monospace",
        size_px: 24.,
        weight: Weight::Bold,
        style: Style::Normal,
        rgb: [0xda, 0xda, 0xda],
    };
    let body = TextStyle {
        family: "monospace",
        size_px: 16.,
        weight: Weight::Normal,
        style: Style::Normal,
        rgb: [0xda, 0xda, 0xda],
    };
    let italic = TextStyle {
        style: Style::Italic,
        ..body
    };

    let origin = interior + Size::from((149., 106.));
    let lines: [(&str, &TextStyle, f64, f64); 4] = [
        ("Your windows. In Sweaters", &title, 0., 0.),
        ("Procedural knit borders for niri.", &body, 15., 46.),
        ("Happy New Year! 🎄", &body, 0., 97.),
        ("It looks like carpets, doesn’t it?", &italic, 0., 146.),
    ];
    for (text, style, dx, dy) in lines {
        elements.push(
            label_element(renderer, text, style, 1., origin + Size::from((dx, dy)))
                .with_context(|| format!("error rendering hero text {text:?}"))?,
        );
    }

    // Purple caret before "Procedural".
    elements.push(fill(
        origin + Size::from((0., 46.)),
        Size::from((2., 24.)),
        [0x8a, 0x5c, 0xf5],
    ));
    Ok(())
}

/// One card of the patterns figure: two stacked knit borders sharing one
/// rounded interior. `top`/`bottom` map to the window-rule palettes in
/// resources/knit-patterns.kdl.
struct PatternCard {
    top_name: &'static str,
    top_pattern: KnitPattern,
    top_base: &'static str,
    top_accent: &'static str,
    top_gradient: Option<Gradient>,
    bottom_name: &'static str,
    bottom_pattern: KnitPattern,
    bottom_base: &'static str,
    bottom_accent: &'static str,
    bottom_gradient: Option<Gradient>,
}

/// The README pattern showcase: three cards, each split horizontally between
/// two patterns over one continuous light interior. Geometry and palettes
/// are sampled from the reference asset and resources/knit-patterns.kdl.
fn compose_patterns(renderer: &mut GlesRenderer, output: &Path) -> anyhow::Result<()> {
    let canvas = Size::from((1479., 674.));
    let card_size = Size::from((462., 649.));
    const ORIGIN_X: f64 = 21.;
    const ORIGIN_Y: f64 = 12.;
    const PITCH: f64 = 490.;
    const BORDER: f64 = 64.;
    const INNER_RADIUS: f32 = 24.;
    /// Vertical split between the top and bottom pattern, as a fraction of
    /// the card height (53% in the reference).
    const SPLIT: f64 = 0.53;
    const SCALE: f64 = 1.;

    let gradient = |from: &'static str, to: &'static str, angle: i16| Gradient {
        from: hex(from),
        to: hex(to),
        angle,
        relative_to: niri_config::GradientRelativeTo::Window,
        in_: GradientInterpolation::default(),
    };

    let cards = [
        PatternCard {
            top_name: "Stockinette",
            top_pattern: KnitPattern::Stockinette,
            top_base: "#796952",
            top_accent: "#f5e2b8",
            top_gradient: None,
            bottom_name: "Zigzag",
            bottom_pattern: KnitPattern::Zigzag,
            bottom_base: "#796952",
            bottom_accent: "#b7a78b",
            bottom_gradient: None,
        },
        PatternCard {
            top_name: "Rib",
            top_pattern: KnitPattern::Rib,
            top_base: "#526c89",
            top_accent: "#8fa6bf",
            top_gradient: None,
            bottom_name: "Diamond",
            bottom_pattern: KnitPattern::Diamond,
            bottom_base: "#6c5878",
            bottom_accent: "#aa97b7",
            bottom_gradient: Some(gradient("#6c5878", "#aa97b7", 45)),
        },
        PatternCard {
            top_name: "Checker",
            top_pattern: KnitPattern::Checker,
            top_base: "#5a6b53",
            top_accent: "#9ba98e",
            top_gradient: None,
            bottom_name: "Dots",
            bottom_pattern: KnitPattern::Dots,
            bottom_base: "#526c89",
            bottom_accent: "#8fa6bf",
            bottom_gradient: None,
        },
    ];

    let label = TextStyle {
        family: "monospace",
        size_px: 16.,
        weight: Weight::Normal,
        style: Style::Normal,
        rgb: [0x38, 0x35, 0x2e],
    };

    let mut elements = vec![fill(Point::from((0., 0.)), canvas, CANVAS_RGB)];
    let split_y = (card_size.h * SPLIT).round();
    let inner = Rectangle::new(
        Point::from((BORDER, BORDER)),
        card_size - Size::from((BORDER * 2., BORDER * 2.)),
    );

    for (col, card) in cards.iter().enumerate() {
        let card_loc = Point::from((ORIGIN_X + PITCH * col as f64, ORIGIN_Y));

        // Continuous light interior under both ring halves.
        elements.push(rounded_fill(
            card_loc + inner.loc.to_size(),
            inner.size,
            CornerRadius::from(INNER_RADIUS),
            2.,
            CANVAS_RGB,
        ));

        // Each half is the full card ring rendered into its own texture:
        // the top half keeps the upper part of the ring, the bottom half is
        // the same ring shifted up so its lower part lands below the split.
        for (is_top, name, pattern, base, accent, grad) in [
            (
                true,
                card.top_name,
                card.top_pattern,
                card.top_base,
                card.top_accent,
                card.top_gradient,
            ),
            (
                false,
                card.bottom_name,
                card.bottom_pattern,
                card.bottom_base,
                card.bottom_accent,
                card.bottom_gradient,
            ),
        ] {
            let (tex_h, shift) = if is_top {
                (split_y, 0.)
            } else {
                (card_size.h - split_y, split_y)
            };
            let spec = RenderSpec {
                context: format!(
                    "patterns column={} {}={}",
                    col + 1,
                    if is_top { "top" } else { "bottom" },
                    name.to_lowercase()
                ),
                win_size: inner.size,
                location: Point::from((BORDER, BORDER - shift)),
                border_width: BORDER,
                outer_radius: INNER_RADIUS + BORDER as f32,
                pattern,
                base: hex(base),
                accent: hex(accent),
                stitch_size: 8.,
                gradient: grad,
                relief: 0.65,
                fuzz: 0.15,
                is_active: true,
                scale: SCALE,
                alpha: 1.,
            };
            let ring = ring_elements(renderer, &spec).with_context(|| {
                format!(
                    "failed to render patterns column={} {}",
                    col + 1,
                    spec.context
                )
            })?;

            let size = Size::<i32, Physical>::from((
                (card_size.w * SCALE).round() as i32,
                (tex_h * SCALE).round() as i32,
            ));
            let (texture, _sync) = render_to_texture(
                renderer,
                size,
                Scale::from(SCALE),
                Transform::Normal,
                TARGET_FORMAT,
                ring.into_iter(),
            )
            .with_context(|| format!("error rendering card half {}", spec.context))?;

            let buffer = TextureBuffer::from_texture(
                renderer,
                texture,
                SCALE,
                Transform::Normal,
                Vec::new(),
            );
            elements.push(
                TextureRenderElement::from_texture_buffer(
                    buffer,
                    card_loc + Size::from((0., shift)),
                    1.,
                    None,
                    None,
                    Kind::Unspecified,
                )
                .into(),
            );
        }

        // Labels: top pattern near the interior top, bottom pattern near the
        // interior bottom, both inset like the reference.
        elements.push(
            label_element(
                renderer,
                card.top_name,
                &label,
                SCALE,
                card_loc + inner.loc.to_size() + Size::from((31., 38.)),
            )
            .with_context(|| format!("error rendering label {}", card.top_name))?,
        );
        elements.push(
            label_element(
                renderer,
                card.bottom_name,
                &label,
                SCALE,
                card_loc + inner.loc.to_size() + Size::from((30., 469.)),
            )
            .with_context(|| format!("error rendering label {}", card.bottom_name))?,
        );
    }

    render_figure_webp(renderer, "knit-patterns", canvas, SCALE, elements, output)
}

fn main() -> anyhow::Result<()> {
    // Canonical documentation assets, independent of the invocation directory.
    let docs = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs");
    let wiki_dir = docs.join("wiki/img");
    let assets_dir = docs.join("assets");

    let mut generator = VisualGenerator::new()?;
    generator.generate(&wiki_dir, &assets_dir)
}
