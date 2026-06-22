// Line particle simulation — one workgroup per 64 particles.
//
// Reads SimParams from a uniform buffer at @group(0) @binding(0).
// Reads + writes Particles in a storage buffer at @group(0) @binding(1).
//
// Each frame, every particle accumulates force from each attractor with
// `power > 0`, applies dual drag (pulling when any attractor is active,
// otherwise inertial), and integrates position. New particles fade in over
// `fade_duration` seconds; out-of-bounds particles teleport home.
//
// Attract mode (params.attract_gate == 1, set only by the screensaver driver):
// - Fraction kill: particles with spawn_hash >= attract_fraction fade out and
//   stay dead (early-out below) so the attract field is sparser/calmer.
// - Lifetime respawn: survivors age; past their CPU-seeded lifespan they
//   teleport home (velocity 0, alpha 0 re-fade), so the field continuously
//   self-heals back into the spawn image.
// Both mechanisms are gated off when attract_gate == 0; live (Active)
// behavior is unchanged (age is pinned to 0, nothing else differs).
//
// CPU mirror: step_one in particles/sim_cpu.rs replicates every kernel term
// (including the stationary-spring below); both files must change together.

struct Particle {
    position: vec2<f32>,
    velocity: vec2<f32>,
    original_xy: vec2<f32>,
    alpha: f32,
    age: f32,
    lifespan: f32,
    spawn_hash: f32,
    // Packed RGB8 spawn colour (render-only); the sim never touches it.
    spawn_color: f32,
    _pad: f32,
};

struct Attractor {
    position: vec2<f32>,
    power: f32,
    // Localized influence radius (world units). 0 = unbounded (constant-magnitude
    // pull, v4 parity — what every current attractor uses); > 0 fades the pull
    // to zero by `radius` (generic localized-attractor support).
    radius: f32,
};

const MAX_ATTRACTORS: u32 = 8u;

struct SimParams {
    dt: f32,
    attractor_count: u32,
    pulling_drag_baked: f32,
    inertial_drag_baked: f32,
    size_scale: f32,
    fade_duration: f32,
    constrain_min: vec2<f32>,
    constrain_max: vec2<f32>,
    attract_gate: u32,
    attract_fraction: f32,
    // Attract-mode noise turbulence: amplitude (0 = off), spatial frequency, and
    // animation phase. stationary_constant (v4 home-spring strength; 0 = off)
    // occupies the same slot as the former _turb_pad to keep `attractors` 16-byte
    // aligned. Renamed in lockstep with the Rust SimParams field (Task 5).
    turbulence_amp: f32,
    turbulence_scale: f32,
    turbulence_time: f32,
    stationary_constant: f32,
    attractors: array<Attractor, MAX_ATTRACTORS>,
};

@group(0) @binding(0) var<uniform> params: SimParams;
@group(0) @binding(1) var<storage, read_write> particles: array<Particle>;

// Divergence-free "curl noise" turbulence force at a world-space point.
//
// We build a scalar stream function psi as a sum of two incommensurate sine
// octaves, then take its 2D curl `(d psi/dy, -d psi/dx)`. The curl of any
// scalar field is divergence-free by construction, so the flow has no sources
// or sinks — particles drift and swirl organically but the field never
// collapses inward (the failure mode a plain gradient/noise pull would have).
// The constant `scale` factor from the chain rule (d/dx = scale * d/dX) is
// folded into `turbulence_amp` by the caller rather than applied here.
fn turbulence_force(pos: vec2<f32>, scale: f32, t: f32) -> vec2<f32> {
    let x = pos.x * scale;
    let y = pos.y * scale;
    // Octave 1: base swirl, slow drift.
    let a1 = x + 0.13 * t;
    let b1 = y - 0.11 * t;
    // Octave 2: half-wavelength detail drifting the other way.
    let a2 = 2.0 * x - 0.17 * t;
    let b2 = 2.0 * y + 0.15 * t;
    // psi = sin(a1)cos(b1) + 0.5 sin(a2)cos(b2)
    //   d psi/dX =  cos(a1)cos(b1) +     cos(a2)cos(b2)
    //   d psi/dY = -sin(a1)sin(b1) -     sin(a2)sin(b2)
    // (octave 2's factor-of-2 from the 2X/2Y argument cancels its 0.5 weight).
    let dpsi_dx = cos(a1) * cos(b1) + cos(a2) * cos(b2);
    let dpsi_dy = -sin(a1) * sin(b1) - sin(a2) * sin(b2);
    // curl = (d psi/dy, -d psi/dx).
    return vec2<f32>(dpsi_dy, -dpsi_dx);
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let idx = id.x;
    let count = arrayLength(&particles);
    if (idx >= count) {
        return;
    }
    var p = particles[idx];

    // --- Attract-mode fraction kill (early-out) ---------------------------
    // Dead particles skip the force/drag/integration math entirely. The
    // dispatch still covers the full buffer (so this fade-out runs, and so
    // the survivors keep simulating at their original indices); the render
    // shader collapses alpha-0 quads so dead particles also cost no fill.
    // On wake (attract_gate -> 0) this branch stops taking and the normal
    // fade-in below restores the dead particles over fade_duration seconds.
    let attract = params.attract_gate != 0u;
    if (attract && p.spawn_hash >= params.attract_fraction) {
        if (p.alpha > 0.0 && params.fade_duration > 0.0) {
            p.alpha = max(p.alpha - params.dt / params.fade_duration, 0.0);
            particles[idx] = p;
        }
        return;
    }

    // --- Accumulate force from active attractors -------------------------
    // v4's particleSystem.ts: forceX = power * G * size_scale * dx / distance.
    // That's a CONSTANT-MAGNITUDE force in the unit direction toward the
    // attractor — distance-independent magnitude, only direction varies.
    // (Not inverse-square or inverse-linear; see v4 reference.)
    //
    // `mouse.power * gravity_constant` is already baked into `attractor.power`
    // host-side. Distance uses a small epsilon to avoid division by zero.
    var accel = vec2<f32>(0.0);
    let active_count = min(params.attractor_count, MAX_ATTRACTORS);
    for (var i: u32 = 0u; i < active_count; i = i + 1u) {
        let a = params.attractors[i];
        if (a.power <= 0.0) {
            continue;
        }
        let delta = a.position - p.position;
        let dist = max(length(delta), 1e-6);
        let dir = delta / dist;
        var force_mag = a.power * params.size_scale;
        // Localized attractors (radius > 0) fade their pull to zero by `radius`,
        // so they only tug nearby particles rather than dragging the whole
        // field. radius == 0 keeps the v4-parity unbounded constant-magnitude
        // pull, which is what every current attractor uses (mouse, hands, pulses).
        if (a.radius > 0.0) {
            force_mag = force_mag * (1.0 - smoothstep(0.0, a.radius, dist));
        }
        accel = accel + dir * force_mag;
    }

    // v4 stationary spring (gated; stationary_constant == 0 -> no-op = Line parity).
    if (params.stationary_constant > 0.0) {
        let home = p.original_xy - p.position;
        let home_len = length(home);
        accel = accel + params.stationary_constant * home * home_len;
        if (params.attractor_count == 0u) {
            p.original_xy = p.original_xy - home * 0.05;
        }
    }

    p.velocity = p.velocity + accel * params.dt;

    // --- Drag selection (pulling when any attractor active) --------------
    let drag = select(params.inertial_drag_baked,
                      params.pulling_drag_baked,
                      params.attractor_count > 0u);
    p.velocity = p.velocity * drag;

    // --- Euler integration -----------------------------------------------
    p.position = p.position + p.velocity * params.dt;

    // --- Attract-mode noise turbulence (position advection) --------------
    // A gentle divergence-free flow field (curl noise) advects the particle
    // position directly, so the calm field drifts organically between pulses
    // instead of sitting dead. Advecting position (rather than adding a force)
    // keeps the drift speed constant — `turbulence_amp` px/s — independent of
    // the drag regime, so it cannot build up under the light "pulling" drag a
    // pulse selects, and being divergence-free it never collapses the
    // field. Off (amp == 0) during live interaction — provably inert.
    if (attract && params.turbulence_amp > 0.0) {
        let turb = turbulence_force(p.position, params.turbulence_scale, params.turbulence_time);
        p.position = p.position + turb * params.turbulence_amp * params.dt;
    }

    // --- Constrain to box; reset to original on OOB ----------------------
    let oob = (p.position.x < params.constrain_min.x ||
               p.position.x > params.constrain_max.x ||
               p.position.y < params.constrain_min.y ||
               p.position.y > params.constrain_max.y);
    if (oob) {
        p.position = p.original_xy;
        p.velocity = vec2<f32>(0.0);
        p.alpha = 0.0; // re-fade-in
    }

    // --- Attract-mode lifetime respawn ------------------------------------
    // Survivors age while attract is on; past their CPU-seeded lifespan they
    // reset exactly like an OOB particle (home, still, alpha-0 re-fade), so
    // the image continuously self-heals. Lifespans are per-particle hashed
    // (~10-18 s) so respawns stagger rather than arriving in waves. During
    // Active the age is pinned to 0, making the mechanism provably inert.
    if (attract) {
        p.age = p.age + params.dt;
        if (p.lifespan > 0.0 && p.age >= p.lifespan) {
            p.age = 0.0;
            p.position = p.original_xy;
            p.velocity = vec2<f32>(0.0);
            p.alpha = 0.0; // re-fade-in
        }
    } else {
        p.age = 0.0;
    }

    // --- Fade-in alpha ---------------------------------------------------
    if (p.alpha < 1.0 && params.fade_duration > 0.0) {
        p.alpha = min(1.0, p.alpha + params.dt / params.fade_duration);
    }

    particles[idx] = p;
}
