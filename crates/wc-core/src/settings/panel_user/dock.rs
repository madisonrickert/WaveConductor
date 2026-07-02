//! Dock data model: tabs, per-frame visibility toggles, storage-key routing,
//! and the right-docked panel's geometry.
//!
//! Pure logic and small `Resource` state, kept apart from [`super`]'s egui
//! drawing so the routing/geometry rules ([`tab_for_storage_key`],
//! [`active_sketch_tab`], [`dock_rect`]) and the Advanced-toggle gate
//! ([`field_visible`]) are unit-testable without an egui context.

use bevy::prelude::Resource;

use crate::lifecycle::state::AppState;
use crate::settings::def::{SettingDef, SettingsCategory};

/// Inline stack snapshot of registered settings storage keys. Sized for the
/// expected case of ≤8 settings types per app; spills to the heap above that.
pub(super) type KeySnapshot = smallvec::SmallVec<[&'static str; 8]>;

/// One tab of the consolidated settings dock.
///
/// Each registered settings struct is routed to a tab by its storage key (see
/// [`tab_for_storage_key`]); the dock renders only the sections whose struct
/// maps to the active tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum SettingsTab {
    /// Active sketch: sketch-specific knobs (particles, visual, audio, etc.).
    ///
    /// The header label and the settings struct rendered here both depend on
    /// which sketch is running; see [`active_sketch_tab`] and
    /// [`super::draw_dock_header`].
    #[default]
    Sketch,
    /// Hand-tracking provider, Leap, and feel.
    HandTracking,
    /// Interface (overlay) and attract-mode/screensaver display.
    Display,
}

impl SettingsTab {
    /// All tabs in left-to-right display order. The label for
    /// [`SettingsTab::Sketch`] is a static placeholder; the live label
    /// comes from [`active_sketch_tab`] and is substituted at render time
    /// in [`super::draw_dock_header`].
    pub(super) const ORDER: [(SettingsTab, &'static str); 3] = [
        (SettingsTab::Sketch, "LINE"),
        (SettingsTab::HandTracking, "HAND TRACKING"),
        (SettingsTab::Display, "DISPLAY"),
    ];
}

/// The dock's currently selected tab. Persists across frames so the operator's
/// tab choice survives panel close/reopen.
#[derive(Resource, Default)]
pub(super) struct SettingsDockTab(pub(super) SettingsTab);

/// Whether the dock's Advanced toggle is on, revealing `Dev`-category settings
/// inline (rendered dimmer). Persists across frames like the tab selection. The
/// hand-tuning "Feel" sliders are `Dev`-category, so this is what surfaces them
/// on the Hand Tracking tab.
#[derive(Resource, Default)]
pub(super) struct SettingsDockAdvanced(pub(super) bool);

/// Whether a field is visible given the current Advanced toggle: `User` fields
/// always, `Dev` fields only when Advanced is on.
pub(super) fn field_visible(def: &SettingDef, advanced: bool) -> bool {
    match def.category {
        SettingsCategory::User => true,
        SettingsCategory::Dev => advanced,
    }
}

/// Route a settings struct (identified by its storage key) to its dock tab.
///
/// The map is intentionally total: any key not explicitly placed — including
/// the overlay (`auto_fade`) and any future settings struct — falls to
/// [`SettingsTab::Display`], so a newly registered struct is always reachable
/// rather than silently hidden.
pub(super) fn tab_for_storage_key(key: &str) -> SettingsTab {
    match key {
        // All sketch settings types route to the generic Sketch tab; only
        // the currently-running sketch's struct renders (see the active-sketch
        // gate in `super::draw_user_panel`).
        "line" | "dots" | "cymatics" => SettingsTab::Sketch,
        "hand_tracking" => SettingsTab::HandTracking,
        // "screensaver", overlay/auto_fade, and anything new.
        _ => SettingsTab::Display,
    }
}

/// Storage key + header label of the settings struct shown in the active-sketch
/// tab, by current `AppState`. Sketches whose settings route to `SettingsTab::Sketch`
/// are gated on this so only the running sketch's knobs render (both `LineSettings`
/// and `DotsSettings` are always registered).
pub(super) fn active_sketch_tab(state: AppState) -> (&'static str, &'static str) {
    match state {
        AppState::Dots => ("dots", "FABRIC"),
        AppState::Cymatics => ("cymatics", "CYMATICS"),
        // Line is the default/home-adjacent sketch label. NOTE: other sketch
        // states (e.g. Flame, Waves) share this fallback and will mis-show the
        // Line tab once they register their own settings — add an arm each here.
        _ => ("line", "LINE"),
    }
}

/// Geometry of the right-docked settings panel for a window of `window_w` ×
/// `window_h` egui points, returned as `(x, y, width, height)`.
///
/// The dock occupies the right half as a zone, capped to a readable 640 px and
/// floored at 420 px so it never collapses narrower than the file-picker rows
/// need; it is inset 16 px from the right and bottom edges and sits 60 px from
/// the top (below the Home/Settings/Volume button strip). Below ~888 px window
/// width the floor wins and the dock may cross the midline — the operator-on-a-
/// laptop case, accepted rather than special-cased.
pub(super) fn dock_rect(window_w: f32, window_h: f32) -> (f32, f32, f32, f32) {
    let width = ((window_w * 0.5) - 24.0).clamp(420.0, 640.0);
    let x = window_w - 16.0 - width;
    let y = 60.0;
    let height = (window_h - 60.0 - 16.0).max(0.0);
    (x, y, width, height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::def::SettingKind;

    /// Both sketch storage keys ("line" and "dots") route to `SettingsTab::Sketch`;
    /// hand-tracking routes to `HandTracking`; everything else falls to `Display`.
    #[test]
    fn dots_and_line_route_to_sketch_tab() {
        assert_eq!(tab_for_storage_key("line"), SettingsTab::Sketch);
        assert_eq!(tab_for_storage_key("dots"), SettingsTab::Sketch);
        assert_eq!(tab_for_storage_key("cymatics"), SettingsTab::Sketch);
        assert_eq!(
            tab_for_storage_key("hand_tracking"),
            SettingsTab::HandTracking
        );
        assert_eq!(tab_for_storage_key("overlay_ui"), SettingsTab::Display);
    }

    /// The active-sketch tab label and storage key follow `AppState`: Dots shows
    /// "FABRIC", Cymatics shows "CYMATICS", and the remaining states (including
    /// Line and Home) fall back to "LINE".
    #[test]
    fn active_sketch_tab_label_follows_state() {
        assert_eq!(active_sketch_tab(AppState::Dots), ("dots", "FABRIC"));
        assert_eq!(active_sketch_tab(AppState::Line), ("line", "LINE"));
        assert_eq!(
            active_sketch_tab(AppState::Cymatics),
            ("cymatics", "CYMATICS")
        );
    }

    /// Every settings struct lands in a tab, and the map is total: unknown
    /// keys (a future struct, the overlay) fall to Display rather than vanish.
    #[test]
    fn tab_routing_is_total() {
        assert_eq!(tab_for_storage_key("line"), SettingsTab::Sketch);
        assert_eq!(
            tab_for_storage_key("hand_tracking"),
            SettingsTab::HandTracking
        );
        assert_eq!(tab_for_storage_key("screensaver"), SettingsTab::Display);
        assert_eq!(tab_for_storage_key("overlay"), SettingsTab::Display);
        assert_eq!(
            tab_for_storage_key("some_future_sketch"),
            SettingsTab::Display,
            "unrecognized keys must route to Display, never disappear"
        );
    }

    /// Dock geometry: right-anchored, capped at 640, floored at 420, inset
    /// 16/16/60 from right/bottom/top.
    #[test]
    #[allow(clippy::float_cmp, reason = "exact arithmetic on integer-valued f32")]
    fn dock_rect_anchors_right_and_clamps_width() {
        // 1080p: half is 936, capped to 640; x = 1920 - 16 - 640.
        let (x, y, w, h) = dock_rect(1920.0, 1080.0);
        assert_eq!(w, 640.0);
        assert_eq!(x, 1920.0 - 16.0 - 640.0);
        assert_eq!(y, 60.0);
        assert_eq!(h, 1080.0 - 76.0);

        // Narrow window: half-24 floors at 420 and the dock may cross center.
        let (xn, _, wn, _) = dock_rect(800.0, 600.0);
        assert_eq!(wn, 420.0, "width floors at 420");
        assert_eq!(xn, 800.0 - 16.0 - 420.0);

        // Mid width that lands inside the band: 1200*0.5-24 = 576.
        let (_, _, wm, _) = dock_rect(1200.0, 800.0);
        assert_eq!(wm, 576.0);

        // Degenerate short window cannot produce a negative height.
        let (_, _, _, hz) = dock_rect(1920.0, 40.0);
        assert!(hz >= 0.0, "height is floored at 0");
    }

    /// Advanced gates Dev fields: User always visible, Dev only when on.
    #[test]
    fn field_visible_gates_dev_on_advanced() {
        let mk = |category| SettingDef {
            field_name: "f",
            label: "F",
            unit: "",
            section: "",
            category,
            kind: SettingKind::Boolean,
            requires_restart: false,
        };
        let user = mk(SettingsCategory::User);
        let dev = mk(SettingsCategory::Dev);
        assert!(field_visible(&user, false), "User visible without advanced");
        assert!(field_visible(&user, true), "User visible with advanced");
        assert!(!field_visible(&dev, false), "Dev hidden without advanced");
        assert!(field_visible(&dev, true), "Dev visible with advanced");
    }
}
