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

struct Particle {
    position: vec2<f32>,
    velocity: vec2<f32>,
    original_xy: vec2<f32>,
    alpha: f32,
    age: f32,
    lifespan: f32,
    spawn_hash: f32,
    _pad: vec2<f32>,
};

struct Attractor {
    position: vec2<f32>,
    power: f32,
    _pad: f32,
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
    attractors: array<Attractor, MAX_ATTRACTORS>,
};

@group(0) @binding(0) var<uniform> params: SimParams;
@group(0) @binding(1) var<storage, read_write> particles: array<Particle>;

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
        let force_mag = a.power * params.size_scale;
        accel = accel + dir * force_mag;
    }
    p.velocity = p.velocity + accel * params.dt;

    // --- Drag selection (pulling when any attractor active) --------------
    let drag = select(params.inertial_drag_baked,
                      params.pulling_drag_baked,
                      params.attractor_count > 0u);
    p.velocity = p.velocity * drag;

    // --- Euler integration -----------------------------------------------
    p.position = p.position + p.velocity * params.dt;

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
    // (~20-45 s) so respawns stagger rather than arriving in waves. During
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
