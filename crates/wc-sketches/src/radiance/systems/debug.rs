//! Radiance dev/debug drivers: the synthetic capture dancer (debug builds),
//! the edge-point gizmo overlay, the inference readout, and the person-cycle
//! hotkey (an operator control for the hardware session — pairs with the
//! readout's people-detected count).
//!
//! The egui readout is registered in `EguiPrimaryContextPass` and self-gates
//! (flame's ui.rs idiom); the gizmo overlay runs `sketch_active` and
//! early-outs on the settings bool. The synthetic dancer runs only under
//! `WC_DEBUG_FORCE_RADIANCE_SYNTHETIC_BODY` in debug builds — it overwrites
//! the mask/edges/body-state/audio resources with deterministic
//! virtual-time data so `cargo xtask capture radiance-*` needs no hardware.
//!
//! Consumes `wc_core::input::body`, which wc-core gates behind the
//! `body-tracking-mediapipe` feature (camera-independent, CI-testable
//! headless). The `cargo doc` gate builds default features only, so this
//! module must be absent there — see `Cargo.toml`'s `body-tracking-mediapipe`
//! forwarding feature, and `radiance::systems::spawn`/`radiance::systems::sim_params`
//! for the identical precedent. The synthetic-dancer driver and its run
//! condition are additionally `#[cfg(debug_assertions)]` (release builds
//! carry no `DebugToggles` resource at all); the gizmo overlay and readout
//! run in every build and self-gate on the always-present `RadianceSettings`
//! Dev bools instead.

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};
#[cfg(debug_assertions)]
use wc_core::input::body::{BodyLandmark, MaskTexture, BODY_LANDMARK_COUNT};
use wc_core::input::body::{BodyTrackingState, SilhouetteEdges};
use wc_core::lifecycle::state::AppState;

use crate::radiance::settings::RadianceSettings;
#[cfg(debug_assertions)]
use crate::radiance::systems::sim_params::IMPULSE_LANDMARKS;

/// Run condition: the synthetic-body capture toggle is set (debug builds).
#[cfg(debug_assertions)]
pub fn synthetic_body_forced(toggles: Option<Res<'_, wc_core::debug::DebugToggles>>) -> bool {
    toggles.is_some_and(|t| t.force_radiance_synthetic_body)
}

/// Build a fully-visible synthetic `TrackedBody` from a phantom pose: the
/// seven impulse landmarks with finite-difference velocities against
/// `pose_prev` (one 60 Hz step earlier).
#[cfg(debug_assertions)]
fn synthetic_tracked_body(
    pose: &crate::radiance::synthetic::PhantomPose,
    pose_prev: &crate::radiance::synthetic::PhantomPose,
    slot: usize,
    fade: f32,
    timestamp: std::time::Duration,
) -> wc_core::input::body::TrackedBody {
    use crate::radiance::synthetic::dancer_landmark_uv;

    let uv_now = dancer_landmark_uv(pose);
    let uv_prev = dancer_landmark_uv(pose_prev);
    let h = 1.0 / 60.0;
    let mut landmarks = [BodyLandmark::default(); BODY_LANDMARK_COUNT];
    let mut velocities = [Vec3::ZERO; BODY_LANDMARK_COUNT];
    for (i, &lm_index) in IMPULSE_LANDMARKS.iter().enumerate() {
        landmarks[lm_index] = BodyLandmark {
            pos: Vec3::new(uv_now[i].x, uv_now[i].y, 0.0),
            visibility: 1.0,
        };
        let v = (uv_now[i] - uv_prev[i]) / h;
        velocities[lm_index] = Vec3::new(v.x, v.y, 0.0);
    }
    wc_core::input::body::TrackedBody {
        slot,
        present: true,
        fade,
        confidence: 1.0,
        landmarks,
        world_landmarks: [Vec3::ZERO; BODY_LANDMARK_COUNT],
        velocities,
        timestamp,
        crop_fraction: 1.0,
        size: 0.25,
        // The phantom dancers oscillate continuously, so pin full motion:
        // the background-subdue grace then contributes an exact 1.0 factor
        // and capture baselines stay byte-stable at any knob setting.
        motion: wc_core::input::body::selection::MOTION_SPEED_HI,
    }
}

/// `Update` (debug builds, `sketch_active(Radiance)` + the toggle, ordered
/// before the live baker): drive the deterministic dancer. Writes the mask +
/// edge list every frame (fixed-dt capture wants per-frame freshness, and
/// thermal budget is irrelevant under capture), synthesizes the seven
/// impulse landmarks with finite-difference velocities, and overwrites
/// `AudioAnalysis` with the synthetic beat (running in `Update` after Plan
/// A's `PreUpdate` publisher means this write wins for the baker).
///
/// Under `WC_DEBUG_FORCE_RADIANCE_SYNTHETIC_DUO` a second dancer (slot 1,
/// offset pose/phase) enters at `synthetic::DUO_ENTRY_T` with a ramped
/// fade, exercising the multi-body identity + ignite paths deterministically.
#[cfg(debug_assertions)]
#[allow(
    clippy::too_many_arguments,
    reason = "Bevy system — the duo toggle adds the DebugToggles read to the \
              existing surface-driver parameters"
)]
pub fn drive_synthetic_body(
    time: Res<'_, Time>,
    mask: Option<Res<'_, MaskTexture>>,
    mut images: ResMut<'_, Assets<Image>>,
    edges: Option<ResMut<'_, SilhouetteEdges>>,
    body: Option<ResMut<'_, BodyTrackingState>>,
    audio: Option<ResMut<'_, wc_core::audio::input::AudioAnalysis>>,
    toggles: Option<Res<'_, wc_core::debug::DebugToggles>>,
    mut commands: Commands<'_, '_>,
) {
    use crate::radiance::synthetic::{
        dancing_pose, duo_partner_fade, duo_partner_pose, duo_primary_pose, extract_edges_slots,
        rasterize_mask_slots, synthetic_audio, DUO_ENTRY_T,
    };

    let t = time.elapsed_secs();
    let duo = toggles.is_some_and(|tg| tg.force_radiance_synthetic_duo);
    let step = 1.0 / 60.0;
    let pose0 = if duo {
        duo_primary_pose(t)
    } else {
        dancing_pose(t)
    };
    let pose0_prev = if duo {
        duo_primary_pose(t - step)
    } else {
        dancing_pose(t - step)
    };
    // The duo partner exists (mask + body) only after its timed entry; its
    // synthetic fade then ramps like the real tracker's attack envelope.
    let partner = (duo && t >= DUO_ENTRY_T).then(|| duo_partner_pose(t));
    let partner_fade = duo_partner_fade(t);

    // Mask + edges through the same shared surfaces the real tracker uses:
    // slot 0 = channel R, slot 1 = channel G, slot-ordered edge list.
    if let (Some(mask), Some(mut edges)) = (mask, edges) {
        if let Some(mut image) = images.get_mut(&mask.0) {
            if let Some(data) = image.data.as_mut() {
                rasterize_mask_slots([Some(&pose0), partner.as_ref(), None, None], data);
                let mut slot_counts = [0_usize; wc_core::input::body::MAX_TRACKED_BODIES];
                let edges = &mut *edges;
                extract_edges_slots(data, &mut edges.points, &mut slot_counts);
                edges.slot_counts = slot_counts;
                edges.generation = edges.generation.wrapping_add(1);
            }
        }
    }

    let mut state = BodyTrackingState::default();
    state.bodies[0] = Some(synthetic_tracked_body(
        &pose0,
        &pose0_prev,
        0,
        1.0,
        time.elapsed(),
    ));
    if partner.is_some() {
        state.bodies[1] = Some(synthetic_tracked_body(
            &duo_partner_pose(t),
            &duo_partner_pose(t - step),
            1,
            partner_fade,
            time.elapsed(),
        ));
    }
    state.primary = Some(0);
    match body {
        Some(mut existing) => *existing = state,
        None => commands.insert_resource(state),
    }

    let frame = synthetic_audio(t);
    match audio {
        Some(mut existing) => *existing = frame,
        None => commands.insert_resource(frame),
    }
}

/// `Update` (`sketch_active(Radiance)`): gizmo tick + outward normal at each
/// edge point (the `edge_debug` Dev toggle). Early-outs on the bool.
pub fn draw_edge_debug(
    settings: Res<'_, RadianceSettings>,
    edges: Option<Res<'_, SilhouetteEdges>>,
    window: Single<'_, '_, &Window>,
    mut gizmos: Gizmos<'_, '_>,
) {
    if !settings.edge_debug {
        return;
    }
    let Some(edges) = edges else {
        return;
    };
    let scale = Vec2::new(window.width().max(1.0), window.height().max(1.0));
    for e in &edges.points {
        let pos =
            crate::radiance::systems::sim_params::mask_uv_to_world(e.pos, scale, settings.mirror);
        let dir = crate::radiance::systems::sim_params::mask_dir_to_world(
            e.normal,
            scale,
            settings.mirror,
        )
        .normalize_or_zero();
        gizmos.line_2d(pos, pos + dir * 12.0, Color::srgb(0.2, 1.0, 0.6));
    }
}

/// `Update` (`sketch_active(Radiance)`): the person-cycle hotkey.
/// `KeyCode::KeyN` ("next person") asks the body worker to cycle its track to
/// the next detected dancer on its next processed frame. Scoped to the active
/// Radiance sketch (not the screensaver) by its run condition; a no-op when no
/// worker is running or only one person is in frame. Reads
/// `ButtonInput<KeyCode>` directly (a sketch-local control, not a global
/// `ActionMap` navigation action) — mirroring Line's direct mouse reads.
pub fn cycle_person_hotkey(
    keys: Res<'_, ButtonInput<KeyCode>>,
    worker: Option<Res<'_, wc_core::input::body::systems::BodyTrackingWorker>>,
) {
    if keys.just_pressed(KeyCode::KeyN) {
        if let Some(worker) = worker {
            worker.request_person_cycle();
        }
    }
}

/// `EguiPrimaryContextPass` (self-gated on state + the Dev bool): tracking +
/// audio readouts. Body frame rate is derived from `timestamp` deltas via
/// `Local`s — everything shown is computable from the pinned contract
/// surface alone.
#[allow(
    clippy::too_many_arguments,
    reason = "Bevy system — each parameter is a distinct ECS resource read \
              by one egui window; splitting it would split the readout"
)]
pub fn radiance_inference_readout(
    app_state: Res<'_, State<AppState>>,
    settings: Res<'_, RadianceSettings>,
    body: Option<Res<'_, BodyTrackingState>>,
    body_diag: Option<Res<'_, wc_core::input::body::BodyTrackingDiagnostics>>,
    audio: Option<Res<'_, wc_core::audio::input::AudioAnalysis>>,
    edges: Option<Res<'_, SilhouetteEdges>>,
    mut last_ts: Local<'_, f64>,
    mut fps: Local<'_, f32>,
    mut contexts: EguiContexts<'_, '_>,
) {
    if **app_state != AppState::Radiance || !settings.inference_readouts {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    if let Some(primary) = body.as_ref().and_then(|b| b.primary()) {
        let ts = primary.timestamp.as_secs_f64();
        let dt = ts - *last_ts;
        if dt > 1e-6 {
            // One-pole smoothed body-frame rate from timestamp deltas.
            #[allow(
                clippy::as_conversions,
                clippy::cast_possible_truncation,
                reason = "display-only smoothing of a bounded dt"
            )]
            {
                *fps = *fps * 0.9 + (1.0 / dt as f32) * 0.1;
            }
            *last_ts = ts;
        }
    }
    egui::Window::new("Radiance readouts")
        .anchor(egui::Align2::LEFT_BOTTOM, egui::vec2(12.0, -12.0))
        .resizable(false)
        .show(ctx, |ui| {
            match body.as_ref() {
                Some(b) => match b.primary() {
                    Some(p) => {
                        ui.label(format!(
                            "bodies: {} primary=slot{} conf={:.2} fade={:.2} crop={:.2} ~{:.1} fps",
                            b.present_count(),
                            p.slot,
                            p.confidence,
                            p.fade,
                            p.crop_fraction,
                            *fps
                        ));
                    }
                    None => {
                        ui.label("bodies: 0 (nobody tracked)");
                    }
                },
                None => {
                    ui.label("body: (no tracking resource)");
                }
            }
            // Worker timing split (same thermal diagnostic the hand provider
            // surfaces): a slow camera/decode reads differently from slow
            // inference on hardware.
            if let Some(d) = body_diag.as_ref() {
                ui.label(format!("worker: {} [{}]", d.status.label(), d.backend));
                ui.label(format!(
                    "timings: cap+dec {:.1}ms pre {:.1}ms det {:.1}ms lm {:.1}ms",
                    d.capture_decode.as_secs_f32() * 1000.0,
                    d.pipeline.preprocess.as_secs_f32() * 1000.0,
                    d.pipeline.detector.as_secs_f32() * 1000.0,
                    d.pipeline.landmark.as_secs_f32() * 1000.0,
                ));
                ui.label(format!(
                    "drops: rate {} ring {} errors {}",
                    d.dropped_frames, d.ring_full_drops, d.pipeline_errors
                ));
                // Candidate people from the most recent detector pass (stale on
                // tracking frames), plus the person-cycle hotkey hint.
                ui.label(format!(
                    "people@detect: {}  [N] next person",
                    d.pipeline.people_detected
                ));
            }
            ui.label(format!(
                "edges: {}",
                edges.as_ref().map_or(0, |e| e.points.len())
            ));
            match audio.as_ref() {
                Some(a) => {
                    ui.label(format!(
                        "audio: active={} rms={:.3} gain={:.2} onset={:.2}",
                        a.active, a.rms, a.gain, a.onset
                    ));
                }
                None => {
                    ui.label("audio: (no analysis resource)");
                }
            }
        });
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use bevy::asset::AssetPlugin;
    use bevy::ecs::system::RunSystemOnce;
    use std::time::Duration;

    use crate::radiance::systems::spawn::ensure_body_surfaces;

    /// The run condition is true only once `force_radiance_synthetic_body`
    /// is set — mirrors the cymatics/dots/line `#[cfg(debug_assertions)]`
    /// gating-predicate tests.
    #[test]
    #[cfg(debug_assertions)]
    fn synthetic_body_forced_only_when_toggle_set() {
        let mut world = World::new();
        let off = world
            .run_system_once(synthetic_body_forced)
            .expect("runs with no toggles resource");
        assert!(!off, "no DebugToggles resource -> forced off");

        world.insert_resource(wc_core::debug::DebugToggles {
            force_g: None,
            disable_smear: false,
            disable_explode: false,
            disable_heatmap_refine: false,
            disable_bloom: false,
            disable_bone_composite: false,
            disable_bone_camera: false,
            solid_particles: None,
            force_screensaver: false,
            force_tier: None,
            force_cymatics_interaction: false,
            force_flame_warp: false,
            force_flame_camera_pose: false,
            force_radiance_synthetic_body: true,
            force_radiance_synthetic_duo: false,
        });
        let on = world
            .run_system_once(synthetic_body_forced)
            .expect("runs with the toggle set");
        assert!(on, "force_radiance_synthetic_body -> forced on");
    }

    /// The person-cycle hotkey reads keyboard + worker and forwards a cycle
    /// request. With no worker running `request_person_cycle` is a harmless
    /// no-op; the system must still run cleanly (wiring smoke test). The
    /// cycle switching itself is covered at the pipeline level, and the
    /// accessor→tuning forwarding at the wc-core systems level.
    #[test]
    fn cycle_person_hotkey_runs_with_key_down_and_no_worker() {
        let mut world = World::new();
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::KeyN);
        world.insert_resource(keys);
        world.insert_resource(wc_core::input::body::systems::BodyTrackingWorker::default());
        world
            .run_system_once(cycle_person_hotkey)
            .expect("hotkey runs with N down");

        // No key pressed → also a clean no-op.
        world.resource_mut::<ButtonInput<KeyCode>>().clear();
        world
            .run_system_once(cycle_person_hotkey)
            .expect("hotkey runs with no key");
    }

    /// The dancer writes a real mask + fresh edge list (mirrors
    /// `screensaver::tests::phantom_writes_mask_and_edges`), inserts a
    /// present `BodyTrackingState` with the seven impulse landmarks visible
    /// and moving, and inserts an active `AudioAnalysis` frame — all three
    /// absent-resource branches (mask/edges always present via
    /// `ensure_body_surfaces`; body/audio absent here) exercised by one call.
    #[test]
    #[cfg(debug_assertions)]
    fn drive_synthetic_body_writes_mask_edges_body_and_audio() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()));
        app.init_asset::<Image>();
        let mut time = Time::<()>::default();
        time.advance_by(Duration::from_millis(500));
        app.insert_resource(time);
        app.world_mut()
            .run_system_once(ensure_body_surfaces)
            .expect("surfaces");

        let gen_before = app.world().resource::<SilhouetteEdges>().generation;
        app.world_mut()
            .run_system_once(drive_synthetic_body)
            .expect("dancer runs");

        let edges = app.world().resource::<SilhouetteEdges>();
        assert!(edges.generation != gen_before, "generation bumped");
        assert!(!edges.points.is_empty(), "dancer has a rim");

        let mask = app.world().resource::<MaskTexture>().0.clone();
        let images = app.world().resource::<Assets<Image>>();
        let data = images
            .get(&mask)
            .and_then(|i| i.data.as_ref())
            .expect("mask bytes");
        assert!(data.iter().any(|&v| v > 128), "dancer body rasterized");

        let body = app.world().resource::<BodyTrackingState>();
        assert!(body.any_present(), "synthetic body reports present");
        let primary = body.primary().expect("slot 0 is primary");
        assert_eq!(primary.slot, 0, "synthetic dancer lives in slot 0");
        assert!((primary.confidence - 1.0).abs() < f32::EPSILON);
        assert!((primary.fade - 1.0).abs() < f32::EPSILON, "fully faded in");
        let visible_and_moving = IMPULSE_LANDMARKS
            .iter()
            .filter(|&&lm| {
                primary.landmarks[lm].visibility > 0.0 && primary.velocities[lm] != Vec3::ZERO
            })
            .count();
        assert!(
            visible_and_moving >= 4,
            "limbs must actually dance ({visible_and_moving} moved)"
        );

        let audio = app
            .world()
            .resource::<wc_core::audio::input::AudioAnalysis>();
        assert!(audio.active, "synthetic audio reports active");
    }
}
