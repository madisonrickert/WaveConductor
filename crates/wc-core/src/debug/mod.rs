//! Render-stage debug toggles parsed once from the `WC_DEBUG_*` env namespace.
//!
//! ## Role
//!
//! Promotes this sprint's throwaway env-gated render-stage isolation toggles
//! into a first-class resource. Relevant systems/nodes read [`DebugToggles`]
//! instead of calling `std::env` directly (or being patched by hand mid-debug).
//! Toggles consumed by render-graph nodes are mirrored into the render world
//! via [`bevy::render::extract_resource::ExtractResource`] (same pattern as
//! `LinePostParams` / `HandMeshTarget`).
//!
//! ## Activation (Option A hybrid)
//!
//! The module is `#[cfg(debug_assertions)]`-gated by its parent declaration in
//! `lib.rs`. At runtime, [`DebugPlugin`] inserts [`DebugToggles`] ONLY when at
//! least one `WC_DEBUG_*` var is present, so a normal debug run carries no
//! resource at all and every consumer treats `Option<Res<DebugToggles>>::None`
//! as "all toggles off".
//!
//! ## Release safety
//!
//! Compiled out of release. Relies on `debug-assertions = false` in
//! release/soak profiles — never enable assertions there. See the guard comment
//! on `[profile.release]` in the workspace `Cargo.toml`.

use bevy::prelude::*;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};

/// Curated render-stage isolation toggles. Absent (resource not inserted) when
/// no `WC_DEBUG_*` var is set; each consumer treats `None` as all-off.
#[derive(Resource, Debug, Clone, Copy, PartialEq, ExtractResource)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is a distinct, documented render-stage isolation toggle, \
              not a state-machine flag set; a struct of named toggles is clearer \
              than an enum/bitflags here"
)]
pub struct DebugToggles {
    /// `WC_DEBUG_FORCE_G=<f32>`: pin the Line gravity-smear `g_constant`,
    /// eliminating the triangle-wave phase variable.
    pub force_g: Option<f32>,
    /// `WC_DEBUG_DISABLE_SMEAR`: skip the gravity post-process node.
    pub disable_smear: bool,
    /// `WC_DEBUG_DISABLE_EXPLODE`: skip the Dots explode (chromatic-aberration)
    /// post-process node, isolating that pass's full-screen fill-rate cost.
    pub disable_explode: bool,
    /// `WC_DEBUG_DISABLE_BLOOM`: zero/disable the main camera bloom.
    pub disable_bloom: bool,
    /// `WC_DEBUG_DISABLE_HEATMAP_REFINE`: skip the `BlazePose` heatmap landmark
    /// refinement pass in the body pipeline, so the hardware session can A/B the
    /// refined landmarks against the raw regression head. Read on the main side
    /// only for `run.json` serialization; the worker reads the same env var
    /// directly at pipeline build (see `input::body::pipeline::PoseConfig`).
    pub disable_heatmap_refine: bool,
    /// `WC_DEBUG_DISABLE_BONE_COMPOSITE`: skip the bone-composite node.
    pub disable_bone_composite: bool,
    /// `WC_DEBUG_DISABLE_BONE_CAMERA`: do not spawn the off-screen bone camera.
    pub disable_bone_camera: bool,
    /// `WC_DEBUG_SOLID_PARTICLES=<rgba hex>`: render particles as a flat linear
    /// colour (`[r, g, b, a]`, 0..=1). `a > 0` means "active".
    pub solid_particles: Option<[f32; 4]>,
    /// `WC_DEBUG_FORCE_SCREENSAVER`: drive `SketchActivity::Screensaver` at
    /// startup so a capture scenario lands in attract mode without waiting out
    /// the idle timer. Presence = on.
    pub force_screensaver: bool,
    /// `WC_DEBUG_FORCE_TIER=<cool|warm|hot>`: pin the screensaver's
    /// [`crate::lifecycle::thermal::ThermalTier`] so each tier can be captured
    /// deterministically (the real sensor is hardware/load-dependent). `None`
    /// = use the live `ThermalState`. Unparseable value → `None`.
    pub force_tier: Option<crate::lifecycle::thermal::ThermalTier>,
    /// `WC_DEBUG_FORCE_CYMATICS_INTERACTION`: in the `cymatics-interacting`
    /// capture scenario, force the primary centre to be held at UV `(0.5, 0.5)`
    /// so the interaction state machine grows `active_radius` deterministically
    /// without hardware or a real mouse press. Presence = on.
    pub force_cymatics_interaction: bool,
    /// `WC_DEBUG_FORCE_FLAME_WARP`: pin the Flame warp offset to a fixed
    /// `(0.35, -0.2)` for the `flame-warp` capture scenario, deterministically
    /// deforming the attractor without a pointer or hand. Presence = on.
    pub force_flame_warp: bool,
    /// `WC_DEBUG_FORCE_FLAME_CAMERA_POSE`: pins a deterministic non-default
    /// Flame camera pose — zoomed in, panned off-center — so captures
    /// regression-guard the target-aware view matrix. Presence = on.
    pub force_flame_camera_pose: bool,
    /// `WC_DEBUG_FORCE_RADIANCE_SYNTHETIC_BODY`: drive Radiance from the
    /// deterministic synthetic dancer (mask + edges + landmarks + audio)
    /// instead of the mic/camera pipelines, and suppress the
    /// `AudioCaptureRequest`/`BodyTrackingRequest` inserts so a capture run
    /// never opens hardware. Presence = on.
    pub force_radiance_synthetic_body: bool,
}

impl DebugToggles {
    /// Build toggles from a list of `(name, value)` env pairs. Recognises only
    /// the `WC_DEBUG_*` names; unknown names are ignored. Flag toggles are true
    /// whenever their var is present (value ignored). Pure for testability.
    pub fn from_env_vars(vars: &[(String, String)]) -> Self {
        let present = |name: &str| vars.iter().any(|(k, _)| k == name);
        let value = |name: &str| {
            vars.iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        };

        let force_g = value("WC_DEBUG_FORCE_G").and_then(|v| v.trim().parse::<f32>().ok());
        let solid_particles =
            value("WC_DEBUG_SOLID_PARTICLES").and_then(|v| parse_rgba_hex(v.trim()));
        let force_tier = value("WC_DEBUG_FORCE_TIER").and_then(|v| parse_tier(v.trim()));

        Self {
            force_g,
            disable_smear: present("WC_DEBUG_DISABLE_SMEAR"),
            disable_explode: present("WC_DEBUG_DISABLE_EXPLODE"),
            disable_bloom: present("WC_DEBUG_DISABLE_BLOOM"),
            disable_heatmap_refine: present("WC_DEBUG_DISABLE_HEATMAP_REFINE"),
            disable_bone_composite: present("WC_DEBUG_DISABLE_BONE_COMPOSITE"),
            disable_bone_camera: present("WC_DEBUG_DISABLE_BONE_CAMERA"),
            solid_particles,
            force_screensaver: present("WC_DEBUG_FORCE_SCREENSAVER"),
            force_tier,
            force_cymatics_interaction: present("WC_DEBUG_FORCE_CYMATICS_INTERACTION"),
            force_flame_warp: present("WC_DEBUG_FORCE_FLAME_WARP"),
            force_flame_camera_pose: present("WC_DEBUG_FORCE_FLAME_CAMERA_POSE"),
            force_radiance_synthetic_body: present("WC_DEBUG_FORCE_RADIANCE_SYNTHETIC_BODY"),
        }
    }

    /// Read the process environment once and build toggles. Used by
    /// [`DebugPlugin::build`]; the pure [`Self::from_env_vars`] backs the tests.
    fn from_process_env() -> Self {
        let vars: Vec<(String, String)> = std::env::vars()
            .filter(|(k, _)| k.starts_with("WC_DEBUG_"))
            .collect();
        Self::from_env_vars(&vars)
    }
}

/// True if any `WC_DEBUG_*` var is present — the activation predicate.
pub fn any_debug_var_present(vars: &[(String, String)]) -> bool {
    vars.iter().any(|(k, _)| k.starts_with("WC_DEBUG_"))
}

/// Parse a `WC_DEBUG_FORCE_TIER` value (case-insensitive `cool`/`warm`/`hot`)
/// into a [`crate::lifecycle::thermal::ThermalTier`]. Returns `None` for any
/// other input so a typo silently falls back to the live tier.
fn parse_tier(value: &str) -> Option<crate::lifecycle::thermal::ThermalTier> {
    use crate::lifecycle::thermal::ThermalTier;
    match value.to_ascii_lowercase().as_str() {
        "cool" => Some(ThermalTier::Cool),
        "warm" => Some(ThermalTier::Warm),
        "hot" => Some(ThermalTier::Hot),
        _ => None,
    }
}

/// Parse a 6- or 8-digit RGB(A) hex string (no `#`) into linear `[r,g,b,a]` in
/// `0..=1`. 6 digits default alpha to `1.0`. Returns `None` on malformed input.
///
/// Note: the bytes are treated as already-linear channel values (the isolation
/// trick wants a literal flat colour, not an sRGB-decoded one).
fn parse_rgba_hex(hex: &str) -> Option<[f32; 4]> {
    let bytes = match hex.len() {
        6 | 8 => hex,
        _ => return None,
    };
    let component = |i: usize| -> Option<f32> {
        let slice = bytes.get(i..i + 2)?;
        let v = u8::from_str_radix(slice, 16).ok()?;
        Some(f32::from(v) / 255.0)
    };
    let r = component(0)?;
    let g = component(2)?;
    let b = component(4)?;
    let a = if bytes.len() == 8 { component(6)? } else { 1.0 };
    Some([r, g, b, a])
}

/// Inserts [`DebugToggles`] (and its render-world extraction) ONLY when a
/// `WC_DEBUG_*` var is present, then leaves consumers to read the resource.
///
/// ## Signal flow
///
/// Parses the `WC_DEBUG_*` env namespace once at `build` time. When any toggle
/// var is set, inserts [`DebugToggles`] into the main world and registers an
/// [`ExtractResourcePlugin`] so render-graph nodes (gravity smear, bone
/// composite) see the same toggles each frame. When no var is set, inserts
/// nothing — every `Option<Res<DebugToggles>>` consumer sees `None`.
pub struct DebugPlugin;

impl Plugin for DebugPlugin {
    fn build(&self, app: &mut App) {
        let vars: Vec<(String, String)> = std::env::vars()
            .filter(|(k, _)| k.starts_with("WC_DEBUG_"))
            .collect();
        if !any_debug_var_present(&vars) {
            return;
        }
        let toggles = DebugToggles::from_process_env();
        tracing::info!(?toggles, "WC_DEBUG_* active");
        app.insert_resource(toggles);
        app.add_plugins(ExtractResourcePlugin::<DebugToggles>::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_var_present_detects_activation() {
        assert!(!any_debug_var_present(&[]));
        assert!(any_debug_var_present(&[(
            "WC_DEBUG_DISABLE_BLOOM".into(),
            "1".into()
        )]));
        assert!(!any_debug_var_present(&[("WC_OTHER".into(), "1".into())]));
    }

    #[test]
    fn parses_flags_and_values() {
        let vars = vec![
            ("WC_DEBUG_FORCE_G".to_string(), "8000".to_string()),
            ("WC_DEBUG_DISABLE_SMEAR".to_string(), "1".to_string()),
            ("WC_DEBUG_DISABLE_EXPLODE".to_string(), "1".to_string()),
            ("WC_DEBUG_DISABLE_BLOOM".to_string(), String::new()),
            (
                "WC_DEBUG_SOLID_PARTICLES".to_string(),
                "ff00ffff".to_string(),
            ),
        ];
        let t = DebugToggles::from_env_vars(&vars);
        assert_eq!(t.force_g, Some(8000.0));
        assert!(t.disable_smear);
        assert!(t.disable_explode);
        assert!(t.disable_bloom);
        assert!(!t.disable_bone_composite);
        assert_eq!(t.solid_particles, Some([1.0, 0.0, 1.0, 1.0]));
    }

    #[test]
    fn solid_particles_rgb_defaults_alpha_to_one() {
        let vars = vec![("WC_DEBUG_SOLID_PARTICLES".to_string(), "00ff00".to_string())];
        let t = DebugToggles::from_env_vars(&vars);
        assert_eq!(t.solid_particles, Some([0.0, 1.0, 0.0, 1.0]));
    }

    #[test]
    fn bad_hex_yields_none() {
        let vars = vec![("WC_DEBUG_SOLID_PARTICLES".to_string(), "zzz".to_string())];
        let t = DebugToggles::from_env_vars(&vars);
        assert_eq!(t.solid_particles, None);
    }

    #[test]
    fn absent_toggles_default_off() {
        let t = DebugToggles::from_env_vars(&[]);
        assert_eq!(t.force_g, None);
        assert!(!t.disable_smear);
        assert!(!t.disable_explode);
        assert!(!t.disable_bloom);
        assert!(!t.disable_heatmap_refine);
        assert!(!t.disable_bone_composite);
        assert!(!t.disable_bone_camera);
        assert_eq!(t.solid_particles, None);
        assert!(!t.force_cymatics_interaction);
        assert!(!t.force_flame_warp);
        assert!(!t.force_flame_camera_pose);
        assert!(!t.force_radiance_synthetic_body);
    }

    #[test]
    fn force_flame_warp_flag_present() {
        let vars = vec![("WC_DEBUG_FORCE_FLAME_WARP".to_string(), String::new())];
        let t = DebugToggles::from_env_vars(&vars);
        assert!(t.force_flame_warp);
    }

    #[test]
    fn force_flame_camera_pose_flag_present() {
        let vars = vec![(
            "WC_DEBUG_FORCE_FLAME_CAMERA_POSE".to_string(),
            String::new(),
        )];
        let t = DebugToggles::from_env_vars(&vars);
        assert!(t.force_flame_camera_pose);
    }

    #[test]
    fn radiance_synthetic_body_flag_parses_by_presence() {
        let vars = vec![(
            "WC_DEBUG_FORCE_RADIANCE_SYNTHETIC_BODY".to_string(),
            String::new(),
        )];
        let t = DebugToggles::from_env_vars(&vars);
        assert!(t.force_radiance_synthetic_body);
        assert!(!DebugToggles::from_env_vars(&[]).force_radiance_synthetic_body);
    }

    #[test]
    fn disable_heatmap_refine_flag_parses_by_presence() {
        let vars = vec![("WC_DEBUG_DISABLE_HEATMAP_REFINE".to_string(), String::new())];
        let t = DebugToggles::from_env_vars(&vars);
        assert!(t.disable_heatmap_refine);
        assert!(!DebugToggles::from_env_vars(&[]).disable_heatmap_refine);
    }

    #[test]
    fn force_g_bad_value_yields_none() {
        let vars = vec![("WC_DEBUG_FORCE_G".to_string(), "not-a-float".to_string())];
        let t = DebugToggles::from_env_vars(&vars);
        assert_eq!(t.force_g, None);
    }

    #[test]
    fn solid_particles_odd_length_yields_none() {
        // 7 hex digits is neither RGB (6) nor RGBA (8).
        let vars = vec![(
            "WC_DEBUG_SOLID_PARTICLES".to_string(),
            "ff00ff0".to_string(),
        )];
        let t = DebugToggles::from_env_vars(&vars);
        assert_eq!(t.solid_particles, None);
    }
}
