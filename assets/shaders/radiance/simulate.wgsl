// Radiance aura simulation — one workgroup per 64 particles.
//
// Reads SimParams from a uniform buffer at @group(0) @binding(0).
// Reads + writes Particles in a storage buffer at @group(0) @binding(1).
// Reads the silhouette edge list (CPU-extracted where the smoothed person
// mask crosses 0.5) at @group(0) @binding(2). The edge buffer is allocated at
// full MAX_EDGE_POINTS capacity and only the first edge_count entries are
// live, so `% edge_count` indexing never leaves the allocation.
//
// Life cycle: a particle is DEAD when age >= lifespan (a zeroed buffer is all
// dead). Each frame a dead particle rolls a hash against emission_prob; on a
// win it respawns at a hashed edge point, offset along the outward normal,
// with initial velocity = normal * (spawn_speed + burst_speed). Alive
// particles advance under buoyancy + limb impulses + drag, then are advected
// along a divergence-free curl-noise flow. There is no OOB teleport — a
// particle that drifts off-screen simply dies at end of life and respawns on
// the silhouette.
//
// CPU parity: RadianceSimParamsGpu / RadianceParticle / RadianceImpulse in
// crates/wc-sketches/src/radiance/compute/sim_params.rs mirror these structs
// field for field (offset_of! tests lock them); the mask-UV -> world mapping
// below must stay term-for-term identical to
// systems::sim_params::mask_uv_to_world.

struct Particle {
    position: vec2<f32>,
    velocity: vec2<f32>,
    age: f32,
    lifespan: f32,
    seed: f32,
    _pad: f32,
};

// Plan B contract shape: mask-UV position (0..1, y down) + outward unit
// normal in the same space.
struct EdgePoint {
    pos: vec2<f32>,
    normal: vec2<f32>,
};

struct Impulse {
    position: vec2<f32>,
    velocity: vec2<f32>,
    radius: f32,
    gain: f32,
    _pad: vec2<f32>,
};

const MAX_IMPULSES: u32 = 8u;

struct SimParams {
    dt: f32,
    time: f32,
    emission_prob: f32,
    edge_count: u32,
    particle_count: u32,
    spawn_offset: f32,
    spawn_speed: f32,
    burst_speed: f32,
    buoyancy: f32,
    flow_strength: f32,
    curl_scale: f32,
    curl_octaves: u32,
    drag_baked: f32,
    lifespan_min: f32,
    lifespan_max: f32,
    mirror: u32,
    uv_to_world: vec2<f32>,
    impulse_count: u32,
    frame: u32,
    impulses: array<Impulse, MAX_IMPULSES>,
};

@group(0) @binding(0) var<uniform> params: SimParams;
@group(0) @binding(1) var<storage, read_write> particles: array<Particle>;
@group(0) @binding(2) var<storage, read> edges: array<EdgePoint>;

// How strongly a particle inside an impulse radius couples to the limb
// velocity, per second. 6.0 means a particle sitting on a limb reaches ~the
// limb's velocity within a couple of frames without hard-snapping to it.
const IMPULSE_COUPLING: f32 = 6.0;

// PCG-style integer hash (Jarzynski & Olano) — cheap, well-distributed.
fn pcg(v: u32) -> u32 {
    var state = v * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

fn hash2(a: u32, b: u32) -> u32 {
    return pcg(a ^ pcg(b));
}

fn rand01(h: u32) -> f32 {
    // Top 24 bits -> [0, 1). Keeps full float precision.
    return f32(h >> 8u) * (1.0 / 16777216.0);
}

// Divergence-free curl-noise flow at a world-space point: the 2D curl of a
// scalar stream function psi built from sine octaves. Curl of a scalar field
// has zero divergence by construction, so the flow swirls without sources or
// sinks (the shared particle engine's turbulence, generalized to a
// param-driven 1..3 octave sum). Per octave the frequency doubles and the
// weight halves; each octave drifts along its own incommensurate direction so
// the field never visibly loops.
fn curl_flow(pos: vec2<f32>, scale: f32, t: f32, octaves: u32) -> vec2<f32> {
    // Per-octave time-drift directions (incommensurate, matching the shared
    // engine's 0.13/0.11 family).
    var drifts = array<vec2<f32>, 3>(
        vec2<f32>(0.13, -0.11),
        vec2<f32>(-0.17, 0.15),
        vec2<f32>(0.07, 0.19),
    );
    var flow = vec2<f32>(0.0);
    var freq = 1.0;
    var amp = 1.0;
    var total = 0.0;
    let n = clamp(octaves, 1u, 3u);
    for (var i = 0u; i < n; i = i + 1u) {
        let a = pos.x * scale * freq + drifts[i].x * t;
        let b = pos.y * scale * freq + drifts[i].y * t;
        // psi = sin(a)cos(b); curl = (d psi/dy, -d psi/dx). The chain-rule
        // scale*freq factor is folded into flow_strength by the caller.
        let dpsi_dx = cos(a) * cos(b);
        let dpsi_dy = -sin(a) * sin(b);
        flow = flow + vec2<f32>(dpsi_dy, -dpsi_dx) * amp;
        total = total + amp;
        freq = freq * 2.0;
        amp = amp * 0.5;
    }
    return flow / max(total, 1e-4);
}

// Mask-UV (0..1, y down) -> world px (origin center, y up), with the mirror
// flip. MUST stay identical to systems::sim_params::mask_uv_to_world.
fn mask_uv_to_world(uv: vec2<f32>) -> vec2<f32> {
    var u = uv.x;
    if (params.mirror == 1u) {
        u = 1.0 - u;
    }
    return vec2<f32>(
        (u - 0.5) * params.uv_to_world.x,
        (0.5 - uv.y) * params.uv_to_world.y,
    );
}

// Mask-UV direction -> world direction (mirror sign on x, y flip), normalized.
fn mask_dir_to_world(dir: vec2<f32>) -> vec2<f32> {
    var sx = params.uv_to_world.x;
    if (params.mirror == 1u) {
        sx = -sx;
    }
    let d = vec2<f32>(dir.x * sx, -dir.y * params.uv_to_world.y);
    let len = length(d);
    if (len < 1e-6) {
        return vec2<f32>(0.0, 1.0);
    }
    return d / len;
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let idx = id.x;
    let count = min(arrayLength(&particles), params.particle_count);
    if (idx >= count) {
        return;
    }
    var p = particles[idx];

    // --- Dead: roll for an edge respawn --------------------------------
    if (p.age >= p.lifespan) {
        if (params.edge_count == 0u) {
            return; // no silhouette this frame; stay dead, write nothing
        }
        // Salt the roll with the CPU frame counter so a losing particle
        // re-rolls fresh every frame (emission_prob is already rate*dt baked).
        // params.frame is incremented once per bake, so it never aliases the
        // way the old u32(time * 60.0) did when time was pinned or two bakes
        // shared a 1/60 s bucket.
        let frame = params.frame;
        if (rand01(hash2(idx, frame)) >= params.emission_prob) {
            return;
        }
        let e = edges[hash2(idx * 2654435769u, frame) % params.edge_count];
        let n = mask_dir_to_world(e.normal);
        p.position = mask_uv_to_world(e.pos) + n * params.spawn_offset;
        p.velocity = n * (params.spawn_speed + params.burst_speed);
        p.age = 0.0;
        p.lifespan = mix(
            params.lifespan_min,
            params.lifespan_max,
            rand01(hash2(idx, frame ^ 2654435769u)),
        );
        p.seed = rand01(hash2(idx, 2246822519u));
        particles[idx] = p;
        return;
    }

    // --- Alive: forces -> drag -> integrate -> curl advection ----------
    p.age = p.age + params.dt;

    // Buoyancy: constant upward acceleration (world +Y is up).
    var accel = vec2<f32>(0.0, params.buoyancy);

    // Limb impulses: locally-weighted coupling toward each limb's velocity,
    // fading to zero by the slot radius — a fast limb sheds a burst.
    let live_impulses = min(params.impulse_count, MAX_IMPULSES);
    for (var i = 0u; i < live_impulses; i = i + 1u) {
        let imp = params.impulses[i];
        if (imp.gain <= 0.0) {
            continue;
        }
        let dist = length(p.position - imp.position);
        let w = 1.0 - smoothstep(0.0, max(imp.radius, 1.0), dist);
        accel = accel + imp.velocity * (imp.gain * w * IMPULSE_COUPLING);
    }

    p.velocity = p.velocity + accel * params.dt;
    // Framerate-independent drag, baked CPU-side as pow(retention, dt).
    p.velocity = p.velocity * params.drag_baked;
    p.position = p.position + p.velocity * params.dt;

    // Curl advection: position (not force) so the drift speed is exactly
    // flow_strength px/s regardless of the drag regime, and the
    // divergence-free field can never collapse the aura inward.
    if (params.flow_strength > 0.0) {
        let turb = curl_flow(p.position, params.curl_scale, params.time, params.curl_octaves);
        p.position = p.position + turb * params.flow_strength * params.dt;
    }

    particles[idx] = p;
}
