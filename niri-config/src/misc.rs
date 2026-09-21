use knuffel::errors::DecodeError;

use crate::appearance::{Color, WorkspaceShadow, WorkspaceShadowPart, DEFAULT_BACKDROP_COLOR};
use crate::utils::{Flag, MergeWith};
use crate::FloatOrInt;

#[derive(knuffel::Decode, Debug, Clone, PartialEq, Eq)]
pub struct SpawnAtStartup {
    #[knuffel(arguments)]
    pub command: Vec<String>,
}

#[derive(knuffel::Decode, Debug, Clone, PartialEq, Eq)]
pub struct SpawnShAtStartup {
    #[knuffel(argument)]
    pub command: String,
}

#[derive(Debug, PartialEq)]
pub struct Cursor {
    pub xcursor_theme: String,
    pub xcursor_size: u8,
    pub hide_when_typing: bool,
    pub hide_after_inactive_ms: Option<u32>,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            xcursor_theme: String::from("default"),
            xcursor_size: 24,
            hide_when_typing: false,
            hide_after_inactive_ms: None,
        }
    }
}

#[derive(knuffel::Decode, Debug, PartialEq)]
pub struct CursorPart {
    #[knuffel(child, unwrap(argument))]
    pub xcursor_theme: Option<String>,
    #[knuffel(child, unwrap(argument))]
    pub xcursor_size: Option<u8>,
    #[knuffel(child)]
    pub hide_when_typing: Option<Flag>,
    #[knuffel(child, unwrap(argument))]
    pub hide_after_inactive_ms: Option<u32>,
}

impl MergeWith<CursorPart> for Cursor {
    fn merge_with(&mut self, part: &CursorPart) {
        merge_clone!((self, part), xcursor_theme, xcursor_size);
        merge!((self, part), hide_when_typing);
        merge_clone_opt!((self, part), hide_after_inactive_ms);
    }
}

#[derive(knuffel::Decode, Debug, Clone, PartialEq)]
pub struct ScreenshotPath(#[knuffel(argument)] pub Option<String>);

impl Default for ScreenshotPath {
    fn default() -> Self {
        Self(Some(String::from(
            "~/Pictures/Screenshots/Screenshot from %Y-%m-%d %H-%M-%S.png",
        )))
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HotkeyOverlay {
    pub skip_at_startup: bool,
    pub hide_not_bound: bool,
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HotkeyOverlayPart {
    #[knuffel(child)]
    pub skip_at_startup: Option<Flag>,
    #[knuffel(child)]
    pub hide_not_bound: Option<Flag>,
}

impl MergeWith<HotkeyOverlayPart> for HotkeyOverlay {
    fn merge_with(&mut self, part: &HotkeyOverlayPart) {
        merge!((self, part), skip_at_startup, hide_not_bound);
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ConfigNotification {
    pub disable_failed: bool,
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ConfigNotificationPart {
    #[knuffel(child)]
    pub disable_failed: Option<Flag>,
}

impl MergeWith<ConfigNotificationPart> for ConfigNotification {
    fn merge_with(&mut self, part: &ConfigNotificationPart) {
        merge!((self, part), disable_failed);
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Clipboard {
    pub disable_primary: bool,
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardPart {
    #[knuffel(child)]
    pub disable_primary: Option<Flag>,
}

impl MergeWith<ClipboardPart> for Clipboard {
    fn merge_with(&mut self, part: &ClipboardPart) {
        merge!((self, part), disable_primary);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Overview {
    pub zoom: f64,
    pub backdrop_color: Color,
    pub workspace_shadow: WorkspaceShadow,
}

impl Default for Overview {
    fn default() -> Self {
        Self {
            zoom: 0.5,
            backdrop_color: DEFAULT_BACKDROP_COLOR,
            workspace_shadow: WorkspaceShadow::default(),
        }
    }
}

#[derive(knuffel::Decode, Debug, Clone, Copy, PartialEq)]
pub struct OverviewPart {
    #[knuffel(child, unwrap(argument))]
    pub zoom: Option<FloatOrInt<0, 1>>,
    #[knuffel(child)]
    pub backdrop_color: Option<Color>,
    #[knuffel(child)]
    pub workspace_shadow: Option<WorkspaceShadowPart>,
}

impl MergeWith<OverviewPart> for Overview {
    fn merge_with(&mut self, part: &OverviewPart) {
        merge!((self, part), zoom, workspace_shadow);
        merge_clone!((self, part), backdrop_color);
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, PartialEq, Eq)]
pub struct Environment(#[knuffel(children)] pub Vec<EnvironmentVariable>);

#[derive(knuffel::Decode, Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentVariable {
    #[knuffel(node_name)]
    pub name: String,
    #[knuffel(argument)]
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XwaylandSatellite {
    pub off: bool,
    pub path: String,
}

impl Default for XwaylandSatellite {
    fn default() -> Self {
        Self {
            off: false,
            path: String::from("xwayland-satellite"),
        }
    }
}

#[derive(knuffel::Decode, Debug, Clone, PartialEq, Eq)]
pub struct XwaylandSatellitePart {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child, unwrap(argument))]
    pub path: Option<String>,
}

impl MergeWith<XwaylandSatellitePart> for XwaylandSatellite {
    fn merge_with(&mut self, part: &XwaylandSatellitePart) {
        self.off |= part.off;
        if part.on {
            self.off = false;
        }

        merge_clone!((self, part), path);
    }
}

/// Desktop zoom settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Zoom {
    /// Maximum zoom level.
    pub max_zoom: f64,
    /// Multiplicative step factor for zoom-in and zoom-out.
    pub increment_factor: f64,
    /// Fraction of the output size along each axis that the cursor can
    /// traverse before the zoomed viewport starts following it.
    pub deadzone_size: f64,
    /// Minimum speed of the deadzone camera follow, in displayed
    /// output-local logical pixels per second.
    ///
    /// Applied as soon as the displayed cursor leaves the deadzone so that
    /// the follow never decays to a crawl right at the border.
    pub follow_min_speed: f64,
    /// Maximum speed of the deadzone camera follow, in displayed
    /// output-local logical pixels per second.
    ///
    /// Reached when the displayed cursor is at the output edge: the speed
    /// scales with the relative depth of the overshoot between the deadzone
    /// border and the output edge.
    pub follow_max_speed: f64,
    /// Number of fingers of a touchpad pinch gesture that controls the
    /// desktop zoom.
    ///
    /// `None` disables compositor pinch zoom: all pinch gestures are
    /// forwarded to clients. Setting this is an explicit opt-in that hands
    /// matching pinch sequences to the compositor instead of applications.
    pub pinch_fingers: Option<u32>,
    /// Debug visualization of the desktop zoom state.
    pub debug: ZoomDebug,
}

impl Default for Zoom {
    fn default() -> Self {
        Self {
            max_zoom: 10.,
            increment_factor: 1.2,
            deadzone_size: 0.5,
            follow_min_speed: 80.,
            follow_max_speed: 1400.,
            pinch_fingers: None,
            debug: ZoomDebug::default(),
        }
    }
}

/// Debug visualization of the desktop zoom state.
///
/// Draws compositor-side overlays on the physical output only; they never
/// appear in screencasts or screen captures and do not affect zoom tracking.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct ZoomDebug {
    /// Outline the deadzone rectangle that the cursor can traverse before the
    /// zoomed viewport starts following it.
    pub deadzone: bool,
    /// Mark the focal point of the current viewport transform.
    pub focal_point: bool,
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct ZoomDebugPart {
    #[knuffel(child)]
    pub deadzone: Option<Flag>,
    #[knuffel(child)]
    pub focal_point: Option<Flag>,
}

impl MergeWith<ZoomDebugPart> for ZoomDebug {
    fn merge_with(&mut self, part: &ZoomDebugPart) {
        merge!((self, part), deadzone, focal_point);
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct ZoomPart {
    #[knuffel(child, unwrap(argument))]
    pub max_zoom: Option<FloatOrInt<1, { i32::MAX }>>,
    #[knuffel(child, unwrap(argument))]
    pub increment_factor: Option<ZoomIncrementFactor>,
    #[knuffel(child, unwrap(argument))]
    pub deadzone_size: Option<FloatOrInt<0, 1>>,
    #[knuffel(child, unwrap(argument))]
    pub follow_min_speed: Option<ZoomFollowSpeed>,
    #[knuffel(child, unwrap(argument))]
    pub follow_max_speed: Option<ZoomFollowSpeed>,
    #[knuffel(child, unwrap(argument))]
    pub pinch_fingers: Option<PinchFingers>,
    #[knuffel(child)]
    pub debug: Option<ZoomDebugPart>,
}

impl MergeWith<ZoomPart> for Zoom {
    fn merge_with(&mut self, part: &ZoomPart) {
        merge!(
            (self, part),
            max_zoom,
            increment_factor,
            deadzone_size,
            follow_min_speed,
            follow_max_speed,
            pinch_fingers,
            debug
        );

        // The maximum must cover the minimum: a config that merges a lower
        // max over a higher min clamps instead of producing an inverted
        // range. Same-node violations are rejected at decode time.
        self.follow_max_speed = self.follow_max_speed.max(self.follow_min_speed);
    }
}

/// Zoom-in/out step factor: a finite number strictly greater than 1.
///
/// A factor of 1 would make zoom-in and zoom-out no-ops, and a factor below 1
/// would invert them, so unlike [`FloatOrInt`] the lower bound is exclusive.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct ZoomIncrementFactor(pub f64);

/// Deadzone camera follow speed: a finite number strictly greater than 0.
///
/// Speeds are in displayed output-local logical pixels per second. A value
/// of 0 would freeze the follow, so unlike [`FloatOrInt`] the lower bound is
/// exclusive.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct ZoomFollowSpeed(pub f64);

impl MergeWith<ZoomFollowSpeed> for f64 {
    fn merge_with(&mut self, part: &ZoomFollowSpeed) {
        *self = part.0;
    }
}

impl<S: knuffel::traits::ErrorSpan> knuffel::DecodeScalar<S> for ZoomFollowSpeed {
    fn type_check(
        type_name: &Option<knuffel::span::Spanned<knuffel::ast::TypeName, S>>,
        ctx: &mut knuffel::decode::Context<S>,
    ) {
        if let Some(type_name) = &type_name {
            ctx.emit_error(DecodeError::unexpected(
                type_name,
                "type name",
                "no type name expected for this node",
            ));
        }
    }

    fn raw_decode(
        val: &knuffel::span::Spanned<knuffel::ast::Literal, S>,
        ctx: &mut knuffel::decode::Context<S>,
    ) -> Result<Self, DecodeError<S>> {
        let value = match &**val {
            knuffel::ast::Literal::Int(value) => match i32::try_from(value) {
                Ok(v) => f64::from(v),
                Err(e) => {
                    ctx.emit_error(DecodeError::conversion(val, e));
                    return Ok(Self::default());
                }
            },
            knuffel::ast::Literal::Decimal(value) => match f64::try_from(value) {
                Ok(v) => v,
                Err(e) => {
                    ctx.emit_error(DecodeError::conversion(val, e));
                    return Ok(Self::default());
                }
            },
            _ => {
                ctx.emit_error(DecodeError::unsupported(
                    val,
                    "Unsupported value, only numbers are recognized",
                ));
                return Ok(Self::default());
            }
        };

        if value.is_finite() && value > 0. {
            Ok(ZoomFollowSpeed(value))
        } else {
            ctx.emit_error(DecodeError::conversion(val, "value must be greater than 0"));
            Ok(Self::default())
        }
    }
}

impl MergeWith<ZoomIncrementFactor> for f64 {
    fn merge_with(&mut self, part: &ZoomIncrementFactor) {
        *self = part.0;
    }
}

impl<S: knuffel::traits::ErrorSpan> knuffel::DecodeScalar<S> for ZoomIncrementFactor {
    fn type_check(
        type_name: &Option<knuffel::span::Spanned<knuffel::ast::TypeName, S>>,
        ctx: &mut knuffel::decode::Context<S>,
    ) {
        if let Some(type_name) = &type_name {
            ctx.emit_error(DecodeError::unexpected(
                type_name,
                "type name",
                "no type name expected for this node",
            ));
        }
    }

    fn raw_decode(
        val: &knuffel::span::Spanned<knuffel::ast::Literal, S>,
        ctx: &mut knuffel::decode::Context<S>,
    ) -> Result<Self, DecodeError<S>> {
        let value = match &**val {
            knuffel::ast::Literal::Int(value) => match i32::try_from(value) {
                Ok(v) => f64::from(v),
                Err(e) => {
                    ctx.emit_error(DecodeError::conversion(val, e));
                    return Ok(Self::default());
                }
            },
            knuffel::ast::Literal::Decimal(value) => match f64::try_from(value) {
                Ok(v) => v,
                Err(e) => {
                    ctx.emit_error(DecodeError::conversion(val, e));
                    return Ok(Self::default());
                }
            },
            _ => {
                ctx.emit_error(DecodeError::unsupported(
                    val,
                    "Unsupported value, only numbers are recognized",
                ));
                return Ok(Self::default());
            }
        };

        if value.is_finite() && value > 1. {
            Ok(ZoomIncrementFactor(value))
        } else {
            ctx.emit_error(DecodeError::conversion(val, "value must be greater than 1"));
            Ok(Self::default())
        }
    }
}

/// Finger count of a touchpad pinch gesture that controls the desktop zoom.
///
/// A pinch needs at least two fingers, so values below 2 are rejected. There
/// is no upper bound: the input API does not limit the finger count.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct PinchFingers(pub u32);

impl MergeWith<PinchFingers> for Option<u32> {
    fn merge_with(&mut self, part: &PinchFingers) {
        *self = Some(part.0);
    }
}

impl<S: knuffel::traits::ErrorSpan> knuffel::DecodeScalar<S> for PinchFingers {
    fn type_check(
        type_name: &Option<knuffel::span::Spanned<knuffel::ast::TypeName, S>>,
        ctx: &mut knuffel::decode::Context<S>,
    ) {
        if let Some(type_name) = &type_name {
            ctx.emit_error(DecodeError::unexpected(
                type_name,
                "type name",
                "no type name expected for this node",
            ));
        }
    }

    fn raw_decode(
        val: &knuffel::span::Spanned<knuffel::ast::Literal, S>,
        ctx: &mut knuffel::decode::Context<S>,
    ) -> Result<Self, DecodeError<S>> {
        let value = match &**val {
            knuffel::ast::Literal::Int(value) => match u32::try_from(value) {
                Ok(v) => v,
                Err(e) => {
                    ctx.emit_error(DecodeError::conversion(val, e));
                    return Ok(Self::default());
                }
            },
            _ => {
                ctx.emit_error(DecodeError::unsupported(
                    val,
                    "Unsupported value, only integers are recognized",
                ));
                return Ok(Self::default());
            }
        };

        if value >= 2 {
            Ok(PinchFingers(value))
        } else {
            ctx.emit_error(DecodeError::conversion(val, "value must be at least 2"));
            Ok(Self::default())
        }
    }
}
