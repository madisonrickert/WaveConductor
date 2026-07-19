//! Beat-pulse layer: waves of HDR light that radiate outward from the
//! dancer's silhouette edge on every detected beat.
//!
//! ## Data flow
//!
//! The analysis engine's debounced beat lane (`AudioAnalysis::beat_confidence`
//! snaps to 1.0 on a beat and decays exponentially) is the strongest signal a
//! party-room mic delivers, and this module is its dedicated visual consumer.
//! Each rising edge spawns one wave into a fixed ring buffer of
//! [`MAX_PULSES`] slots ([`RadiancePulses`]). A fullscreen additive quad
//! ([`RadiancePulseMaterial`], z 2.0, over the billboards) renders every live
//! slot in `shaders/radiance/pulse.wgsl` as an expanding **iso-distance
//! contour of the silhouette**: the shader samples the chamfer distance
//! field (`super::distance_field`) and lights the band where
//! `distance-from-body ≈ wave radius`, so the front detaches from the
//! dancer's outline and travels outward *keeping the body's shape* — nested
//! silhouettes of light, not circles around a point. At age 0 the band sits
//! at distance 0: the body itself flashes on the beat, then the contour
//! peels off and radiates. Wave strength is **bass-weighted** (the beat lane
//! times the wave, the bass drive weights it), wave color is the
//! fade-weighted blend of the present bodies' identity colors, and the
//! master brightness rides the union presence fade so a wave can never
//! outlive the last figure (see [`union_fade`]).
//!
//! ## Hot-path invariants
//!
//! Fixed-size arrays throughout: per-frame work is a slot walk plus one
//! uniform re-prepare (the `drive_radiance_materials` cost class). Nothing
//! allocates after spawn. During the attract screensaver the mic is paused →
//! `beat_confidence` holds 0 → no spawns; residual waves fade within
//! [`PULSE_LIFETIME_S`] and the master lane is dimmed by the screensaver fade.

use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, BlendComponent, BlendFactor, BlendOperation, BlendState, RenderPipelineDescriptor,
    ShaderType, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;
use bevy::sprite_render::{AlphaMode2d, Material2d, Material2dKey};
use wc_core::audio::input::AudioAnalysis;
use wc_core::input::body::{BodyTrackingState, MASK_SIZE};
use wc_core::lifecycle::screensaver::fade::ScreensaverFade;

use super::distance_field::DIST_MAX_TEXELS;
use super::render::slot_identity_colors;
use super::settings::RadianceSettings;
use super::systems::sim_params::RadianceState;
use super::systems::spawn::RadianceRoot;

/// Fixed pulse slot count (uniform array size; WGSL mirrors it).
pub const MAX_PULSES: usize = 6;
/// Wave expansion speed, world px/s. The front clears a 1080-px-tall screen
/// from the silhouette in under a second — one wave is still visibly
/// travelling when the next beat lands at dance tempi, layering nested
/// silhouette contours.
pub const PULSE_SPEED_PX_S: f32 = 650.0;
/// Base gaussian band half-width, world px (widens as the wave ages).
pub const PULSE_WIDTH_PX: f32 = 60.0;
/// Seconds until a pulse slot is dead (the shader also fades a tail window
/// ending here, so the cutoff is invisible).
pub const PULSE_LIFETIME_S: f32 = 1.6;
/// `beat_confidence` rising-edge threshold that fires a wave. Confidence
/// snaps to 1.0 on a beat and decays with a 0.3 s time constant, so at the
/// 240 BPM debounce ceiling it still falls to ~0.43 between beats — every
/// debounced beat produces exactly one rising edge here.
pub const BEAT_EDGE: f32 = 0.6;
/// Frame-delta cap, matching the sim baker's hitch guard.
const PULSE_DT_CAP: f32 = 0.05;

/// One wave: born on a beat, expanding with age.
#[derive(Clone, Copy, Debug)]
pub struct PulseSlot {
    /// Seconds since the beat that spawned it (`>= PULSE_LIFETIME_S` = dead).
    pub age: f32,
    /// Brightness scale in `0..1` (onset-derived at spawn).
    pub strength: f32,
    /// Linear-HDR wave color (palette-derived at spawn).
    pub color: Vec4,
}

impl Default for PulseSlot {
    /// A dead slot: expired age, zero strength.
    fn default() -> Self {
        Self {
            age: PULSE_LIFETIME_S,
            strength: 0.0,
            color: Vec4::ZERO,
        }
    }
}

/// CPU pulse state: a fixed ring buffer of slots plus the beat edge tracker.
/// Inserted on Radiance entry, removed on exit.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct RadiancePulses {
    /// The slots; spawning overwrites round-robin (oldest-first by index).
    pub slots: [PulseSlot; MAX_PULSES],
    /// Next slot index to overwrite.
    next_slot: usize,
    /// Previous frame's `beat_confidence` (rising-edge detection).
    prev_beat: f32,
}

/// The uniform block the contour-wave shader consumes. WGSL struct parity is
/// by convention (arrays of vec4, stride 16).
#[derive(ShaderType, Clone, Copy, Debug)]
pub struct RadiancePulseUniform {
    /// Per slot: x = age s, y = strength (0 = dead), zw unused.
    pub pulses: [Vec4; MAX_PULSES],
    /// Per slot: rgb = linear-HDR wave color, w unused.
    pub colors: [Vec4; MAX_PULSES],
    /// x = master intensity (pulse setting × screensaver-fade dim),
    /// y = expansion speed px/s, z = base band width px, w = lifetime s.
    pub params: Vec4,
    /// Distance-field mapping: x = mirror (1 = flip), y = fit-to-height
    /// aspect (`window_w/window_h`; 1 = full-window stretch), z = world px
    /// per mask texel, w = [`DIST_MAX_TEXELS`] (R8 denormalization).
    pub mapping: Vec4,
}

impl Default for RadiancePulseUniform {
    /// All slots dead, canonical speed/width/lifetime, master 0.
    fn default() -> Self {
        Self {
            pulses: [Vec4::ZERO; MAX_PULSES],
            colors: [Vec4::ZERO; MAX_PULSES],
            params: Vec4::new(0.0, PULSE_SPEED_PX_S, PULSE_WIDTH_PX, PULSE_LIFETIME_S),
            mapping: Vec4::new(1.0, 1.0, 4.0, DIST_MAX_TEXELS),
        }
    }
}

/// Fullscreen additive material drawing every live silhouette-contour wave
/// (fragment-only; the default `Material2d` vertex shader supplies UVs).
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct RadiancePulseMaterial {
    /// The 256² `R8Unorm` silhouette distance field
    /// (`super::distance_field` recomputes it per body frame).
    #[texture(0)]
    #[sampler(1)]
    pub distance_field: Handle<Image>,
    /// The packed pulse state for this frame.
    #[uniform(2)]
    pub pulses: RadiancePulseUniform,
}

impl Material2d for RadiancePulseMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/radiance/pulse.wgsl".into()
    }

    /// `Blend` routes into `Transparent2d`; [`Self::specialize`] then makes
    /// it pure additive (the `RadianceMaterial` recipe).
    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Blend
    }

    /// Override the color-target blend to pure additive `(One, One)` so the
    /// waves accumulate HDR light into bloom instead of alpha-occluding.
    fn specialize(
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: Material2dKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        if let Some(fragment) = descriptor.fragment.as_mut() {
            if let Some(Some(target)) = fragment.targets.get_mut(0) {
                target.blend = Some(BlendState {
                    color: BlendComponent {
                        src_factor: BlendFactor::One,
                        dst_factor: BlendFactor::One,
                        operation: BlendOperation::Add,
                    },
                    alpha: BlendComponent {
                        src_factor: BlendFactor::One,
                        dst_factor: BlendFactor::One,
                        operation: BlendOperation::Add,
                    },
                });
            }
        }
        Ok(())
    }
}

/// Sample the three-stop palette gradient at `t` (the CPU twin of the render
/// shader's `gradient`).
#[must_use]
pub fn gradient_sample(stops: &[Vec4; 3], t: f32) -> Vec4 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        stops[0].lerp(stops[1], t * 2.0)
    } else {
        stops[1].lerp(stops[2], (t - 0.5) * 2.0)
    }
}

/// Raw per-slot fade vector — **no** phantom fallback (unlike
/// `render::slot_fades`): unoccupied slots are 0, so the union below reads
/// exactly "how present is anybody".
#[must_use]
pub fn raw_slot_fades(body: Option<&BodyTrackingState>) -> Vec4 {
    let mut fades = Vec4::ZERO;
    if let Some(state) = body {
        for b in state.iter_bodies() {
            if b.slot < 4 {
                fades[b.slot] = b.fade.clamp(0.0, 1.0);
            }
        }
    }
    fades
}

/// The union presence envelope: the maximum fade across occupied slots. The
/// pulse master rides it so beat waves can never outlive the last figure —
/// when the final dancer's fade releases, the residual waves dim with it,
/// and an empty room's stale distance field can never flash ghost contours.
#[must_use]
pub fn union_fade(fades: Vec4) -> f32 {
    fades.x.max(fades.y).max(fades.z).max(fades.w)
}

/// Fade-weighted blend of the present bodies' identity colors — the wave
/// color of a mixed floor is the palette blend of everyone dancing. Falls
/// back to `fallback` when nobody carries fade (the wave then rides the
/// plain palette, e.g. the synthetic/phantom writers).
#[must_use]
pub fn blend_present_colors(colors: [Vec4; 4], fades: Vec4, fallback: Vec4) -> Vec4 {
    let sum = fades.x + fades.y + fades.z + fades.w;
    if sum <= f32::EPSILON {
        return fallback;
    }
    (colors[0] * fades.x + colors[1] * fades.y + colors[2] * fades.z + colors[3] * fades.w) / sum
}

/// Advance every slot by `dt` and spawn one wave on a rising beat edge.
/// Returns `true` when a wave was spawned. Pure over its inputs so the
/// beat-edge/round-robin behavior is unit-testable without an app.
pub fn step_pulses(
    pulses: &mut RadiancePulses,
    dt: f32,
    beat_confidence: f32,
    spawn_enabled: bool,
    strength: f32,
    color: Vec4,
) -> bool {
    for slot in &mut pulses.slots {
        slot.age += dt;
    }
    let rising = beat_confidence > BEAT_EDGE && pulses.prev_beat <= BEAT_EDGE;
    pulses.prev_beat = beat_confidence;
    if !(rising && spawn_enabled) {
        return false;
    }
    pulses.slots[pulses.next_slot] = PulseSlot {
        age: 0.0,
        strength: strength.clamp(0.0, 1.0),
        color,
    };
    pulses.next_slot = (pulses.next_slot + 1) % MAX_PULSES;
    true
}

/// Pack the CPU slots into the shader uniform. `master` is the pulse
/// setting × the screensaver-fade dim; dead slots pack zero strength so the
/// shader skips them. `px_per_texel` converts field texels to world px
/// (`uv_to_world.y / MASK_SIZE`); `fit_aspect`/`mirror` mirror the
/// silhouette material's mask mapping so wave and fill agree per pixel.
#[must_use]
pub fn pack_pulse_uniform(
    pulses: &RadiancePulses,
    master: f32,
    mirror: bool,
    fit_aspect: f32,
    px_per_texel: f32,
) -> RadiancePulseUniform {
    let mut uniform = RadiancePulseUniform {
        params: Vec4::new(
            master.max(0.0),
            PULSE_SPEED_PX_S,
            PULSE_WIDTH_PX,
            PULSE_LIFETIME_S,
        ),
        mapping: Vec4::new(
            f32::from(u8::from(mirror)),
            fit_aspect,
            px_per_texel,
            DIST_MAX_TEXELS,
        ),
        ..RadiancePulseUniform::default()
    };
    for (i, slot) in pulses.slots.iter().enumerate() {
        let live = slot.age < PULSE_LIFETIME_S && slot.strength > 0.0;
        uniform.pulses[i] = Vec4::new(slot.age, if live { slot.strength } else { 0.0 }, 0.0, 0.0);
        uniform.colors[i] = slot.color;
    }
    uniform
}

/// `Update` (gated `in_state(AppState::Radiance)`, like the material driver,
/// so residual waves keep fading through Idle/Screensaver): advance + spawn
/// waves from the beat lane and pack the uniform into the pulse material.
#[allow(
    clippy::too_many_arguments,
    reason = "Bevy system — each param is a distinct ECS resource/query the driver packs"
)]
pub fn update_radiance_pulses(
    time: Res<'_, Time>,
    window: Single<'_, '_, &Window>,
    settings: Res<'_, RadianceSettings>,
    state: Res<'_, RadianceState>,
    fade: Res<'_, ScreensaverFade>,
    audio: Option<Res<'_, AudioAnalysis>>,
    body: Option<Res<'_, BodyTrackingState>>,
    mut pulses: ResMut<'_, RadiancePulses>,
    quads: Query<
        '_,
        '_,
        &bevy::sprite_render::MeshMaterial2d<RadiancePulseMaterial>,
        With<RadianceRoot>,
    >,
    mut materials: ResMut<'_, Assets<RadiancePulseMaterial>>,
) {
    let dt = time.delta_secs().min(PULSE_DT_CAP);
    let audio_frame = audio.map_or_else(AudioAnalysis::neutral, |a| *a);

    // Wave color: the fade-weighted palette blend of the present bodies'
    // identity colors (one dancer = their color; a mixed floor = the blend),
    // falling back to slot 0's identity for the body-less writers.
    let slot_colors = slot_identity_colors(
        settings.palette,
        state.hue_phase,
        settings.hue_spread,
        fade.alpha(),
    );
    let fades = raw_slot_fades(body.as_deref());
    let color = blend_present_colors(slot_colors, fades, slot_colors[0]);

    // Strength is bass-weighted (the spec's "big pulses follow the beat" —
    // beat timing from the confidence edge, wave WEIGHT from the bass body);
    // the 0.35 floor keeps soft beats visible.
    let strength = (0.35 + 0.65 * state.bass_drive).clamp(0.0, 1.0);
    let spawn_enabled =
        settings.pulse_intensity > 0.0 && audio_frame.active && settings.audio_sensitivity > 0.0;
    step_pulses(
        &mut pulses,
        dt,
        audio_frame.beat_confidence,
        spawn_enabled,
        strength,
        color,
    );

    // Same mask-mapping terms the silhouette material packs, so the wave's
    // distance lookup agrees with the rendered fill per pixel.
    let h = window.height().max(1.0);
    let fit_aspect = if settings.fit_to_height {
        window.width() / h
    } else {
        1.0
    };
    #[allow(
        clippy::as_conversions,
        clippy::cast_precision_loss,
        reason = "MASK_SIZE = 256, exact in f32"
    )]
    let px_per_texel = h / MASK_SIZE as f32;

    // Master rides the screensaver dim AND the union presence fade: waves
    // never outlive the last figure (see `union_fade`).
    let master = settings.pulse_intensity * (1.0 - fade.alpha()) * union_fade(fades);
    let uniform = pack_pulse_uniform(&pulses, master, settings.mirror, fit_aspect, px_per_texel);
    for handle in &quads {
        if let Some(mut material) = materials.get_mut(&handle.0) {
            material.pulses = uniform;
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;

    /// A rising beat edge spawns exactly one wave; the decaying confidence
    /// tail and a held-high value do not retrigger; the next beat does.
    #[test]
    fn beat_edge_spawns_once_per_beat() {
        let mut pulses = RadiancePulses::default();
        let dt = 1.0 / 60.0;
        let c = Vec4::ONE;
        assert!(step_pulses(&mut pulses, dt, 1.0, true, 0.8, c));
        // Decay tail: still above the edge, but not rising.
        assert!(!step_pulses(&mut pulses, dt, 0.9, true, 0.8, c));
        assert!(!step_pulses(&mut pulses, dt, 0.7, true, 0.8, c));
        // Below the edge, then the next beat snaps it back up.
        assert!(!step_pulses(&mut pulses, dt, 0.3, true, 0.8, c));
        assert!(step_pulses(&mut pulses, dt, 1.0, true, 0.8, c));
        // Exactly two waves total across five frames with two beats: the
        // first (now four frames old) and the fresh one in the next slot.
        let live = pulses
            .slots
            .iter()
            .filter(|s| s.age < PULSE_LIFETIME_S)
            .count();
        assert_eq!(live, 2, "two beats -> two live waves");
        assert!(
            pulses.slots[1].age.abs() < f32::EPSILON,
            "round-robin advanced to slot 1"
        );
    }

    /// Spawns overwrite round-robin without disturbing other slots' ages.
    #[test]
    fn spawns_rotate_through_slots() {
        let mut pulses = RadiancePulses::default();
        for i in 0..MAX_PULSES + 2 {
            // Drop confidence to re-arm the edge, then beat. Tag each spawn
            // by strength so wrap-around is observable.
            #[allow(
                clippy::as_conversions,
                clippy::cast_precision_loss,
                reason = "small loop index, exact in f32"
            )]
            let tag = 0.1 + (i as f32) * 0.1;
            step_pulses(&mut pulses, 0.01, 0.0, true, tag, Vec4::ONE);
            let spawned = step_pulses(&mut pulses, 0.01, 1.0, true, tag, Vec4::ONE);
            assert!(spawned, "beat {i} must spawn");
        }
        // The two wrap-around spawns overwrote slots 0 and 1.
        #[allow(
            clippy::as_conversions,
            clippy::cast_precision_loss,
            reason = "small loop index, exact in f32"
        )]
        {
            let expect0 = 0.1 + (MAX_PULSES as f32) * 0.1;
            let expect1 = 0.1 + ((MAX_PULSES + 1) as f32) * 0.1;
            assert!((pulses.slots[0].strength - expect0).abs() < 1e-6);
            assert!((pulses.slots[1].strength - expect1).abs() < 1e-6);
        }
    }

    /// Disabled spawning (pulse setting 0 / inactive audio) never fires.
    #[test]
    fn disabled_spawning_never_fires() {
        let mut pulses = RadiancePulses::default();
        assert!(!step_pulses(&mut pulses, 0.01, 1.0, false, 1.0, Vec4::ONE));
        assert!(pulses.slots.iter().all(|s| s.strength.abs() < f32::EPSILON));
    }

    /// Dead and expired slots pack zero strength; live slots carry theirs;
    /// the mapping lane carries the mask-transform terms.
    #[test]
    fn pack_zeroes_dead_slots_and_carries_mapping() {
        let mut pulses = RadiancePulses::default();
        step_pulses(&mut pulses, 0.01, 1.0, true, 0.9, Vec4::ONE);
        let packed = pack_pulse_uniform(&pulses, 1.5, true, 1.78, 4.2);
        assert!(
            (packed.pulses[0].y - 0.9).abs() < f32::EPSILON,
            "live slot keeps strength"
        );
        for slot in &packed.pulses[1..] {
            assert!(slot.y.abs() < f32::EPSILON, "dead slots pack zero strength");
        }
        assert!(
            (packed.params.x - 1.5).abs() < f32::EPSILON,
            "master in params.x"
        );
        assert!((packed.mapping.x - 1.0).abs() < f32::EPSILON, "mirror flag");
        assert!((packed.mapping.y - 1.78).abs() < f32::EPSILON, "fit aspect");
        assert!(
            (packed.mapping.z - 4.2).abs() < f32::EPSILON,
            "px per texel"
        );
        assert!(
            (packed.mapping.w - DIST_MAX_TEXELS).abs() < f32::EPSILON,
            "denormalization"
        );
        // Age past the lifetime: packs dead.
        for _ in 0..200 {
            step_pulses(&mut pulses, 0.05, 0.0, true, 0.9, Vec4::ONE);
        }
        let packed = pack_pulse_uniform(&pulses, 1.0, false, 1.0, 4.0);
        assert!(
            packed.pulses[0].y.abs() < f32::EPSILON,
            "expired slot packs dead"
        );
    }

    /// Union fade is the max across occupied slots; raw fades carry no
    /// phantom fallback (empty state = 0 — the ghost-wave suppressor).
    #[test]
    fn union_fade_tracks_occupied_slots_only() {
        use wc_core::input::body::{BodyTrackingState, TrackedBody};
        assert!(union_fade(raw_slot_fades(None)).abs() < f32::EPSILON);
        let empty = BodyTrackingState::default();
        assert!(
            union_fade(raw_slot_fades(Some(&empty))).abs() < f32::EPSILON,
            "no bodies -> no pulse master (unlike the fill's phantom fallback)"
        );
        let mut state = BodyTrackingState::default();
        state.bodies[2] = Some(TrackedBody {
            slot: 2,
            present: false, // fading out
            fade: 0.4,
            ..TrackedBody::default()
        });
        let fades = raw_slot_fades(Some(&state));
        assert_eq!(fades, Vec4::new(0.0, 0.0, 0.4, 0.0));
        assert!((union_fade(fades) - 0.4).abs() < f32::EPSILON);
    }

    /// Wave color is the fade-weighted blend of present identities; nobody
    /// present falls back to the given color.
    #[test]
    fn blend_present_colors_weights_by_fade() {
        let colors = [
            Vec4::new(1.0, 0.0, 0.0, 1.0),
            Vec4::new(0.0, 1.0, 0.0, 1.0),
            Vec4::ZERO,
            Vec4::ZERO,
        ];
        let fallback = Vec4::new(0.5, 0.5, 0.5, 1.0);
        assert_eq!(
            blend_present_colors(colors, Vec4::ZERO, fallback),
            fallback,
            "nobody present -> fallback"
        );
        let blended = blend_present_colors(colors, Vec4::new(1.0, 1.0, 0.0, 0.0), fallback);
        assert!((blended.x - 0.5).abs() < 1e-6 && (blended.y - 0.5).abs() < 1e-6);
        let solo = blend_present_colors(colors, Vec4::new(0.3, 0.0, 0.0, 0.0), fallback);
        assert_eq!(solo, colors[0], "solo dancer keeps their exact identity");
    }

    /// Gradient sampling hits the stops at 0 / 0.5 / 1 and clamps outside.
    #[test]
    fn gradient_sample_interpolates_three_stops() {
        let stops = [
            Vec4::new(1.0, 0.0, 0.0, 1.0),
            Vec4::new(0.0, 1.0, 0.0, 1.0),
            Vec4::new(0.0, 0.0, 1.0, 1.0),
        ];
        assert_eq!(gradient_sample(&stops, 0.0), stops[0]);
        assert_eq!(gradient_sample(&stops, 0.5), stops[1]);
        assert_eq!(gradient_sample(&stops, 1.0), stops[2]);
        assert_eq!(gradient_sample(&stops, -1.0), stops[0]);
        assert_eq!(gradient_sample(&stops, 2.0), stops[2]);
        let q = gradient_sample(&stops, 0.25);
        assert!((q.x - 0.5).abs() < 1e-6 && (q.y - 0.5).abs() < 1e-6);
    }
}
