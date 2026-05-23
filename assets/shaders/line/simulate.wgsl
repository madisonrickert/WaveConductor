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
    if (params.attractor_enabled > 0.5) {
        let to_attr = params.attractor_pos - p.position;
        // Clamp denominator to attractor_radius^2 so force is bounded inside the radius.
        let dist_sq = max(dot(to_attr, to_attr), params.attractor_radius * params.attractor_radius);
        let inv_dist = 1.0 / sqrt(dist_sq);
        // Gravitational acceleration = gravity_constant / dist^2, applied along the unit vector.
        let force = params.gravity_constant * inv_dist * inv_dist;
        p.velocity = p.velocity + to_attr * inv_dist * force * params.dt;
    }

    // Apply drag (exponential decay approximation for small dt).
    p.velocity = p.velocity * (1.0 - params.drag * params.dt);

    // Euler integration.
    p.position = p.position + p.velocity * params.dt;

    particles[idx] = p;
}
