//! GTK control panel for the Knit Playground.
//!
//! `build()` returns a horizontal box: the `SmithayView` preview on the left
//! (expanding) and a scrolled ~300px settings panel on the right. All controls
//! write into the `SharedKnitState` handed to the scene factory, so the state
//! survives the scene being destroyed on unmap and recreated on map.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};
use niri_config::{Color, KnitPattern};

use crate::cases::knit::Knit;
use crate::cases::knit_settings::{KnitSettings, KnitState, SharedKnitState};
use crate::smithay_view::SmithayView;

const PATTERNS: [(KnitPattern, &str); 6] = [
    (KnitPattern::Stockinette, "Stockinette"),
    (KnitPattern::Rib, "Rib"),
    (KnitPattern::Checker, "Checker"),
    (KnitPattern::Zigzag, "Zigzag"),
    (KnitPattern::Diamond, "Diamond"),
    (KnitPattern::Dots, "Dots"),
];

#[derive(Clone, Copy)]
enum Axis {
    Width,
    Height,
    Radius,
}

impl Axis {
    fn name(self) -> &'static str {
        match self {
            Axis::Width => "Width",
            Axis::Height => "Height",
            Axis::Radius => "Corner radius",
        }
    }

    fn percent(self, s: &KnitSettings) -> f64 {
        match self {
            Axis::Width => s.width,
            Axis::Height => s.height,
            Axis::Radius => s.radius,
        }
    }

    fn set_percent(self, s: &mut KnitSettings, v: f64) {
        match self {
            Axis::Width => s.width = v,
            Axis::Height => s.height = v,
            Axis::Radius => s.radius = v,
        }
    }

    fn auto(self, s: &KnitSettings) -> bool {
        match self {
            Axis::Width => s.auto_width,
            Axis::Height => s.auto_height,
            Axis::Radius => s.auto_radius,
        }
    }

    fn set_auto(self, s: &mut KnitSettings, v: bool) {
        match self {
            Axis::Width => s.auto_width = v,
            Axis::Height => s.auto_height = v,
            Axis::Radius => s.auto_radius = v,
        }
    }

    /// Actual size in physical pixels reported back by the scene.
    fn px(self, window_size: (i32, i32), corner_radius: f64) -> f64 {
        match self {
            Axis::Width => window_size.0 as f64,
            Axis::Height => window_size.1 as f64,
            Axis::Radius => corner_radius,
        }
    }
}

#[derive(Clone, Copy)]
enum ColorField {
    Background,
    Inner,
    Base,
    Accent,
}

impl ColorField {
    fn name(self) -> &'static str {
        match self {
            ColorField::Background => "Background",
            ColorField::Inner => "Inner",
            ColorField::Base => "Base",
            ColorField::Accent => "Accent",
        }
    }

    fn get(self, s: &KnitSettings) -> Color {
        match self {
            ColorField::Background => s.background,
            ColorField::Inner => s.inner,
            ColorField::Base => s.base,
            ColorField::Accent => s.accent,
        }
    }

    fn set(self, s: &mut KnitSettings, c: Color) {
        match self {
            ColorField::Background => s.background = c,
            ColorField::Inner => s.inner = c,
            ColorField::Base => s.base = c,
            ColorField::Accent => s.accent = c,
        }
    }

    /// The background is opaque; the other colors allow alpha.
    fn opaque(self) -> bool {
        matches!(self, ColorField::Background)
    }

    fn hex_hint(self) -> &'static str {
        if self.opaque() {
            "#RRGGBB"
        } else {
            "#RRGGBBAA"
        }
    }
}

fn to_rgba(c: Color) -> gdk::RGBA {
    gdk::RGBA::new(c.r, c.g, c.b, c.a)
}

fn from_rgba(rgba: gdk::RGBA, opaque: bool) -> Color {
    let a = if opaque { 1. } else { rgba.alpha() };
    Color::new_unpremul(rgba.red(), rgba.green(), rgba.blue(), a)
}

fn format_hex(c: Color, opaque: bool) -> String {
    let byte = |x: f32| (x.clamp(0., 1.) * 255.).round() as u8;
    if opaque {
        format!("#{:02X}{:02X}{:02X}", byte(c.r), byte(c.g), byte(c.b))
    } else {
        format!(
            "#{:02X}{:02X}{:02X}{:02X}",
            byte(c.r),
            byte(c.g),
            byte(c.b),
            byte(c.a)
        )
    }
}

fn parse_hex(text: &str, opaque: bool) -> Option<Color> {
    let text = text.trim();
    let digits = text.strip_prefix('#')?;
    let len_ok = if opaque {
        digits.len() == 6
    } else {
        digits.len() == 6 || digits.len() == 8
    };
    if !len_ok || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    gdk::RGBA::parse(text)
        .ok()
        .map(|rgba| from_rgba(rgba, opaque))
}

fn set_a11y_label(widget: &impl IsA<gtk::Accessible>, label: &str) {
    widget.update_property(&[gtk::accessible::Property::Label(label)]);
}

struct NumericRow {
    adjustment: gtk::Adjustment,
    scale: gtk::Scale,
    spin: gtk::SpinButton,
}

/// A row with a title, a slider and a spin button sharing one adjustment.
fn numeric_row(
    name: &str,
    min: f64,
    max: f64,
    step: f64,
    digits: u32,
    extra_top: &[&gtk::Widget],
) -> (gtk::Box, NumericRow) {
    let adjustment = gtk::Adjustment::new(0., min, max, step, step * 10., 0.);

    let label = gtk::Label::new(Some(name));
    label.set_xalign(0.);
    label.set_hexpand(true);

    let spin = gtk::SpinButton::new(Some(&adjustment), step, digits);
    spin.set_numeric(true);
    set_a11y_label(&spin, name);

    let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    top.append(&label);
    for w in extra_top {
        top.append(*w);
    }
    top.append(&spin);

    let scale = gtk::Scale::new(gtk::Orientation::Horizontal, Some(&adjustment));
    scale.set_hexpand(true);
    set_a11y_label(&scale, name);

    let row = gtk::Box::new(gtk::Orientation::Vertical, 6);
    row.set_margin_top(6);
    row.set_margin_bottom(6);
    row.set_margin_start(12);
    row.set_margin_end(12);
    row.append(&top);
    row.append(&scale);

    (
        row,
        NumericRow {
            adjustment,
            scale,
            spin,
        },
    )
}

struct GeometryRow {
    axis: Axis,
    numeric: NumericRow,
    px_label: gtk::Label,
    last_px: Cell<Option<i64>>,
    auto_switch: gtk::Switch,
}

/// A percent slider+spin row with an actual-px label and an independent
/// "Auto" switch that animates the axis.
fn geometry_row(axis: Axis) -> (gtk::Box, GeometryRow) {
    let name = axis.name();

    let px_label = gtk::Label::new(Some("–"));
    px_label.add_css_class("dim-label");

    let auto_label = gtk::Label::new(Some("Auto"));
    auto_label.add_css_class("dim-label");

    let auto_switch = gtk::Switch::new();
    auto_switch.set_valign(gtk::Align::Center);
    set_a11y_label(&auto_switch, &format!("Auto {name}"));

    let (row, numeric) = numeric_row(
        name,
        0.,
        100.,
        1.,
        0,
        &[
            px_label.upcast_ref::<gtk::Widget>(),
            auto_label.upcast_ref::<gtk::Widget>(),
            auto_switch.upcast_ref::<gtk::Widget>(),
        ],
    );

    (
        row,
        GeometryRow {
            axis,
            numeric,
            px_label,
            last_px: Cell::new(None),
            auto_switch,
        },
    )
}

struct ColorRow {
    field: ColorField,
    button: gtk::ColorDialogButton,
    entry: gtk::Entry,
}

/// A row with a color picker button and an editable HEX entry.
fn color_row(field: ColorField) -> (adw::ActionRow, ColorRow) {
    let name = field.name();

    let dialog = gtk::ColorDialog::new();
    dialog.set_title(name);
    dialog.set_with_alpha(!field.opaque());
    let button = gtk::ColorDialogButton::new(Some(dialog));
    set_a11y_label(&button, &format!("{name} color"));

    let entry = gtk::Entry::new();
    entry.set_max_length(9);
    entry.set_width_chars(9);
    entry.set_placeholder_text(Some(field.hex_hint()));
    set_a11y_label(&entry, &format!("{name} hex"));

    let row = adw::ActionRow::new();
    row.set_title(name);
    row.add_suffix(&button);
    row.add_suffix(&entry);
    row.set_activatable_widget(Some(&button));

    (
        row,
        ColorRow {
            field,
            button,
            entry,
        },
    )
}

struct Panel {
    state: SharedKnitState,
    view: SmithayView,
    /// Set while syncing widgets from state so that the resulting signal
    /// emissions don't write back into the state or queue renders.
    syncing: Cell<bool>,
    enabled: gtk::Switch,
    pattern: gtk::DropDown,
    border_width: NumericRow,
    stitch_size: NumericRow,
    relief: NumericRow,
    fuzz: NumericRow,
    geometry: [GeometryRow; 3],
    colors: [ColorRow; 4],
}

impl Panel {
    /// Push the current settings into every widget. Emitted signals are
    /// swallowed by `syncing`.
    fn sync_from_settings(&self) {
        self.syncing.set(true);
        self.sync_widgets();
        self.syncing.set(false);
    }

    fn sync_widgets(&self) {
        // Copy the snapshot out before emitting any GTK signals.
        let settings = self.state.borrow().settings;

        self.enabled.set_active(settings.enabled);
        self.pattern.set_selected(
            PATTERNS
                .iter()
                .position(|(p, _)| *p == settings.pattern)
                .unwrap_or(0) as u32,
        );
        self.border_width
            .adjustment
            .set_value(settings.border_width);
        self.stitch_size.adjustment.set_value(settings.stitch_size);
        self.relief.adjustment.set_value(settings.relief);
        self.fuzz.adjustment.set_value(settings.fuzz);
        for row in &self.geometry {
            row.numeric
                .adjustment
                .set_value(row.axis.percent(&settings));
            row.auto_switch.set_active(row.axis.auto(&settings));
            let manual = !row.axis.auto(&settings);
            row.numeric.scale.set_sensitive(manual);
            row.numeric.spin.set_sensitive(manual);
        }
        for row in &self.colors {
            let color = row.field.get(&settings);
            row.button.set_rgba(&to_rgba(color));
            row.entry.set_text(&format_hex(color, row.field.opaque()));
            row.entry.remove_css_class("error");
            row.entry.set_tooltip_text(None);
        }
    }

    /// Per-frame feedback: while an axis is animated by the scene, reflect the
    /// live percent in the (disabled) manual controls and always show the
    /// actual pixel size. Never writes back into the state.
    fn sync_feedback(&self) {
        let (settings, window_size, corner_radius) = {
            let state = self.state.borrow();
            (state.settings, state.window_size, state.corner_radius)
        };

        self.syncing.set(true);
        for row in &self.geometry {
            let auto = row.axis.auto(&settings);
            row.numeric.scale.set_sensitive(!auto);
            row.numeric.spin.set_sensitive(!auto);
            let percent = row.axis.percent(&settings);
            if (row.numeric.adjustment.value() - percent).abs() > 1e-9 {
                row.numeric.adjustment.set_value(percent);
            }
            let px = row.axis.px(window_size, corner_radius).round() as i64;
            if row.last_px.replace(Some(px)) != Some(px) {
                row.px_label.set_label(&format!("{px} px"));
            }
        }
        self.syncing.set(false);
    }

    fn reset(&self) {
        self.state.borrow_mut().settings = KnitSettings::default();
        self.sync_from_settings();
        self.view.queue_render();
    }

    fn connect(panel: &Rc<Panel>, reset: &gtk::Button) {
        let weak = Rc::downgrade(panel);

        panel.enabled.connect_active_notify({
            let panel = weak.clone();
            move |switch| {
                let Some(panel) = panel.upgrade() else { return };
                if panel.syncing.get() {
                    return;
                }
                panel.state.borrow_mut().settings.enabled = switch.is_active();
                panel.view.queue_render();
            }
        });

        panel.pattern.connect_selected_notify({
            let panel = weak.clone();
            move |dropdown| {
                let Some(panel) = panel.upgrade() else { return };
                if panel.syncing.get() {
                    return;
                }
                if let Some((pattern, _)) = PATTERNS.get(dropdown.selected() as usize) {
                    panel.state.borrow_mut().settings.pattern = *pattern;
                    panel.view.queue_render();
                }
            }
        });

        let connect_numeric = |row: &NumericRow, write: fn(&mut KnitSettings, f64)| {
            row.adjustment.connect_value_changed({
                let panel = weak.clone();
                move |adj| {
                    let Some(panel) = panel.upgrade() else { return };
                    if panel.syncing.get() {
                        return;
                    }
                    write(&mut panel.state.borrow_mut().settings, adj.value());
                    panel.view.queue_render();
                }
            });
        };
        connect_numeric(&panel.border_width, |s, v| s.border_width = v);
        connect_numeric(&panel.stitch_size, |s, v| s.stitch_size = v);
        connect_numeric(&panel.relief, |s, v| s.relief = v);
        connect_numeric(&panel.fuzz, |s, v| s.fuzz = v);

        for row in &panel.geometry {
            let axis = row.axis;
            row.numeric.adjustment.connect_value_changed({
                let panel = weak.clone();
                move |adj| {
                    let Some(panel) = panel.upgrade() else { return };
                    if panel.syncing.get() {
                        return;
                    }
                    axis.set_percent(&mut panel.state.borrow_mut().settings, adj.value());
                    panel.view.queue_render();
                }
            });
            row.auto_switch.connect_active_notify({
                let panel = weak.clone();
                let scale = row.numeric.scale.clone();
                let spin = row.numeric.spin.clone();
                move |switch| {
                    let Some(panel) = panel.upgrade() else { return };
                    if panel.syncing.get() {
                        return;
                    }
                    let active = switch.is_active();
                    axis.set_auto(&mut panel.state.borrow_mut().settings, active);
                    scale.set_sensitive(!active);
                    spin.set_sensitive(!active);
                    panel.view.queue_render();
                }
            });
        }

        for row in &panel.colors {
            let field = row.field;

            row.button.connect_rgba_notify({
                let panel = weak.clone();
                let entry = row.entry.downgrade();
                move |button| {
                    let Some(panel) = panel.upgrade() else { return };
                    if panel.syncing.get() {
                        return;
                    }
                    let color = from_rgba(button.rgba(), field.opaque());
                    field.set(&mut panel.state.borrow_mut().settings, color);
                    if let Some(entry) = entry.upgrade() {
                        panel.syncing.set(true);
                        entry.set_text(&format_hex(color, field.opaque()));
                        entry.remove_css_class("error");
                        entry.set_tooltip_text(None);
                        panel.syncing.set(false);
                    }
                    panel.view.queue_render();
                }
            });

            row.entry.connect_changed({
                let panel = weak.clone();
                let button = row.button.downgrade();
                move |entry| {
                    let Some(panel) = panel.upgrade() else { return };
                    if panel.syncing.get() {
                        return;
                    }
                    match parse_hex(&entry.text(), field.opaque()) {
                        Some(color) => {
                            entry.remove_css_class("error");
                            entry.set_tooltip_text(None);
                            field.set(&mut panel.state.borrow_mut().settings, color);
                            if let Some(button) = button.upgrade() {
                                panel.syncing.set(true);
                                button.set_rgba(&to_rgba(color));
                                panel.syncing.set(false);
                            }
                            panel.view.queue_render();
                        }
                        None => {
                            // Invalid input only flags the entry; the state
                            // keeps the last valid color.
                            entry.add_css_class("error");
                            entry.set_tooltip_text(Some(
                                "Invalid hex color, expected RRGGBB or RRGGBBAA",
                            ));
                        }
                    }
                }
            });
        }

        reset.connect_clicked({
            let panel = weak.clone();
            move |_| {
                let Some(panel) = panel.upgrade() else { return };
                panel.reset();
            }
        });
    }
}

pub fn build(anim_adjustment: &gtk::Adjustment) -> gtk::Box {
    let state: SharedKnitState = Rc::new(RefCell::new(KnitState::default()));

    let view = SmithayView::new(
        {
            let state = state.clone();
            move |args| Knit::new(args, state.clone())
        },
        anim_adjustment,
    );
    view.set_hexpand(true);
    view.set_vexpand(true);

    // Material.
    let enabled = gtk::Switch::new();
    enabled.set_valign(gtk::Align::Center);
    set_a11y_label(&enabled, "Knit border");
    let enabled_row = adw::ActionRow::new();
    enabled_row.set_title("Knit border");
    enabled_row.add_suffix(&enabled);
    enabled_row.set_activatable_widget(Some(&enabled));
    let pattern = gtk::DropDown::from_strings(&PATTERNS.map(|(_, name)| name));
    set_a11y_label(&pattern, "Pattern");
    let pattern_row = adw::ActionRow::new();
    pattern_row.set_title("Pattern");
    pattern_row.add_suffix(&pattern);
    pattern_row.set_activatable_widget(Some(&pattern));

    let material_group = adw::PreferencesGroup::new();
    material_group.set_title("Material");
    material_group.add(&enabled_row);
    material_group.add(&pattern_row);

    // Knit.
    let (border_row, border_width) = numeric_row("Border width", 1., 128., 1., 0, &[]);
    let (stitch_row, stitch_size) = numeric_row("Stitch size", 1., 64., 0.25, 2, &[]);
    let (relief_row, relief) = numeric_row("Relief", 0., 1., 0.01, 2, &[]);
    let (fuzz_row, fuzz) = numeric_row("Fuzz", 0., 1., 0.01, 2, &[]);

    let knit_group = adw::PreferencesGroup::new();
    knit_group.set_title("Knit");
    knit_group.add(&border_row);
    knit_group.add(&stitch_row);
    knit_group.add(&relief_row);
    knit_group.add(&fuzz_row);

    // Window geometry.
    let (width_row, width) = geometry_row(Axis::Width);
    let (height_row, height) = geometry_row(Axis::Height);
    let (radius_row, radius) = geometry_row(Axis::Radius);

    let window_group = adw::PreferencesGroup::new();
    window_group.set_title("Window");
    window_group.add(&width_row);
    window_group.add(&height_row);
    window_group.add(&radius_row);

    // Colors.
    let (background_row, background) = color_row(ColorField::Background);
    let (inner_row, inner) = color_row(ColorField::Inner);
    let (base_row, base) = color_row(ColorField::Base);
    let (accent_row, accent) = color_row(ColorField::Accent);

    let colors_group = adw::PreferencesGroup::new();
    colors_group.set_title("Colors");
    colors_group.add(&background_row);
    colors_group.add(&inner_row);
    colors_group.add(&base_row);
    colors_group.add(&accent_row);

    let reset = gtk::Button::with_label("Reset to Defaults");
    reset.set_margin_top(6);

    let panel_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    panel_box.set_margin_top(12);
    panel_box.set_margin_bottom(12);
    panel_box.set_margin_start(12);
    panel_box.set_margin_end(12);
    panel_box.append(&material_group);
    panel_box.append(&knit_group);
    panel_box.append(&window_group);
    panel_box.append(&colors_group);
    panel_box.append(&reset);

    let scrolled = gtk::ScrolledWindow::new();
    scrolled.set_hscrollbar_policy(gtk::PolicyType::Never);
    scrolled.set_hexpand(false);
    scrolled.set_min_content_width(300);
    scrolled.set_max_content_width(300);
    scrolled.set_propagate_natural_width(true);
    scrolled.set_child(Some(&panel_box));

    let panel = Rc::new(Panel {
        state,
        view: view.clone(),
        syncing: Cell::new(false),
        enabled,
        pattern,
        border_width,
        stitch_size,
        relief,
        fuzz,
        geometry: [width, height, radius],
        colors: [background, inner, base, accent],
    });

    Panel::connect(&panel, &reset);
    panel.sync_from_settings();

    let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    root.append(&view);
    root.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    root.append(&scrolled);

    // Sync the live animated values and actual pixel sizes back into the
    // controls. The callback is owned by `root`, which keeps the panel alive
    // for exactly as long as the UI exists; it pauses while unmapped.
    let _tick = root.add_tick_callback(move |_, _| {
        panel.sync_feedback();
        glib::ControlFlow::Continue
    });

    root
}
