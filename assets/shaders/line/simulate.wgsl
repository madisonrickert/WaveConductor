// Line particle simulation — one workgroup per 64 particles.
//
// Reads SimParams from a uniform buffer at @group(0) @binding(0).
// Reads + writes Particles in a storage buffer at @group(0) @binding(1).

struct Particle {
    position: vec2<f32>,
    velocity: vec2<f32>,
};

struct SimParams {
    dt: f32,
    drag: f32,
    attractor_pos: vec2<f32>,
    attractor_radius: f32,
    gravity_constant: f32,
    attractor_enabled: f32,
    _pad: f32,
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

    // Gravity toward the attractor.
    //
    // Inverse-linear pull rather than inverse-square: at screen-pixel scales
    // (distances in the hundreds of px), inverse-square underflows to
    // negligible acceleration with reasonable G values. The 1/dist falloff
    // gives visible motion across the whole window while still preferring
    // closer particles.
    //
    // `attractor_radius` defines the "core" — distance is clamped to at
    // least `attractor_radius`, so force inside the radius is bounded and
    // particles don't fling out due to a near-zero denominator.
    if (params.attractor_enabled > 0.5) {
        let to_attr = params.attractor_pos - p.position;
        let raw_dist = length(to_attr);
        let safe_dist = max(raw_dist, 0.001);
        let effective_dist = max(raw_dist, params.attractor_radius);
        let dir = to_attr / safe_dist;
        // Acceleration ≈ gravity_constant * attractor_radius / dist (px/s²).
        // The attractor_radius factor keeps the units in "px/s²" regardless
        // of how the user tunes radius vs gravity_constant.
        let acceleration = params.gravity_constant * params.attractor_radius / effective_dist;
        p.velocity = p.velocity + dir * acceleration * params.dt;
    }

    // Apply drag. `drag` is enforced 0..1 by LineSettings::drag (Rust side);
    // the shader does not clamp here.
    p.velocity = p.velocity * (1.0 - params.drag * params.dt);

    // Euler integration.
    p.position = p.position + p.velocity * params.dt;

    particles[idx] = p;
}
