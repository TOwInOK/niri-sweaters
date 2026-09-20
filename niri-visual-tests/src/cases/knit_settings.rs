use std::cell::RefCell;
use std::rc::Rc;

use niri_config::{Color, KnitPattern};

/// Live knit playground settings shared between the GTK panel and the scene.
///
/// `width`, `height` and `radius` are normalized to `0..=100` percent of their
/// respective ranges: content size spans `1px..=available` (viewport minus
/// border and margin), corner radius spans `0..=half` the smaller content side.
///
/// While an `auto_*` flag is set, the scene writes the animated percent back
/// into the corresponding field every frame, so disabling auto freezes at the
/// last displayed value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KnitSettings {
    /// Whether the knit shader is drawn. When off, the plain border remains.
    pub enabled: bool,
    pub pattern: KnitPattern,
    pub border_width: f64,
    pub stitch_size: f64,
    pub relief: f64,
    pub fuzz: f64,
    pub width: f64,
    pub height: f64,
    pub radius: f64,
    pub auto_width: bool,
    pub auto_height: bool,
    pub auto_radius: bool,
    /// Opaque backdrop behind the window.
    pub background: Color,
    /// Window contents color.
    pub inner: Color,
    /// Border base color.
    pub base: Color,
    /// Knit accent color.
    pub accent: Color,
}

impl Default for KnitSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            pattern: KnitPattern::Stockinette,
            border_width: 48.,
            stitch_size: 8.,
            relief: 0.65,
            fuzz: 0.15,
            width: 65.,
            height: 65.,
            radius: 20.,
            auto_width: false,
            auto_height: false,
            auto_radius: false,
            background: Color::from_array_unpremul([0.3, 0.3, 0.3, 1.]),
            inner: Color::from_array_unpremul([0.15, 0.64, 0.41, 1.]),
            base: Color::from_rgba8_unpremul(0x52, 0x6c, 0x89, 0xff),
            accent: Color::from_rgba8_unpremul(0x8f, 0xa6, 0xbf, 0xff),
        }
    }
}

/// Shared state that survives scene destruction (unmap/remap).
///
/// `window_size` and `corner_radius` are feedback written by the scene: the
/// actual content size and applied corner radius in logical pixels.
#[derive(Debug, Default)]
pub struct KnitState {
    pub settings: KnitSettings,
    pub window_size: (i32, i32),
    pub corner_radius: f64,
}

pub type SharedKnitState = Rc<RefCell<KnitState>>;
