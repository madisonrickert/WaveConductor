// Radiance aura simulation — one workgroup per 64 particles.
//
// Reads SimParams from a uniform buffer at @group(0) @binding(0).
// Reads + writes Particles in a storage buffer at @group(0) @binding(1).
// Reads the silhouette edge list (CPU-extracted where the smoothed person
// mask crosses 0.5) at @group(0) @binding(2). The edge buffer is allocated at
// full MAX_EDGE_POINTS capacity; the CPU packs per-slot (start, count) ranges
// so indexing `start + hash % count` never leaves a slot's live prefix.
//
// Life cycle: a particle is DEAD when age >= lifespan (a zeroed buffer is all
// dead). Each frame a dead particle rolls a hash against emission_prob; on a
// win it picks a BODY SLOT from the fade-weighted emission CDF (slot_cdf), a
// hashed edge point inside that slot's edge range, and respawns there offset
// along the outward normal. A second roll against ejecta_prob decides whether
// this spawn is a fast "shooting" streak (onset-driven, high normal velocity,
// short life) or an ordinary flame particle. Alive particles advance under
// tongue-modulated buoyancy + limb impulses + drag, then are advected along a
// divergence-free curl-noise flow. There is no OOB teleport — a particle that
// drifts off-screen simply dies at end of life and respawns on a silhouette.
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
    // Body slot index (0..4) stored as f32; the render shader rounds it back
    // to index the per-slot color array.
    slot: f32,
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
    // Per-slot edge ranges + fade-weighted emission CDF (multi-body spawn
    // apportioning; see the CPU baker).
    slot_start: vec4<u32>,
    slot_count: vec4<u32>,
    slot_cdf: vec4<f32>,
    // Onset-driven shooting-spark layer + flame-tongue buoyancy noise.
    ejecta_prob: f32,
    ejecta_speed: f32,
    tongue_amp: f32,
    tongue_freq: f32,
    impulses: array<Impulse, MAX_IMPULSES>,
};

@group(0) @binding(0) var<uniform> params: SimParams;
@group(0) @binding(1) var<storage, read_write> particles: array<Particle>;
@group(0) @binding(2) var<storage, read> edges: array<EdgePoint>;

// How strongly a particle inside an impulse radius couples to the limb
// velocity, per second. 6.0 means a particle sitting on a limb reaches ~the
// limb's velocity within a couple of frames without hard-snapping to it.
const IMPULSE_COUPLING: f32 = 6.0;
// Ejecta lifespan multiplier: shooting sparks die young, so the streaks stay
// crisp instead of loitering as slow embers far from the body.
const EJECTA_LIFE_MUL: f32 = 0.3;

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

// Pick the spawn slot from the fade-weighted emission CDF: the first entry
// the roll falls under. Returns 4 (invalid) when every weight is zero, which
// the caller treats as "no live body — stay dead".
fn pick_slot(r: f32) -> u32 {
    for (var i = 0u; i < 4u; i = i + 1u) {
        if (r < params.slot_cdf[i]) {
            return i;
        }
    }
    return 4u;
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
        let frame = params.frame;
        if (rand01(hash2(idx, frame)) >= params.emission_prob) {
            return;
        }
        // Fade-weighted slot pick: the shared budget is apportioned across
        // the live bodies (density stays constant as dancers come and go).
        let slot = pick_slot(rand01(hash2(idx ^ 0x9e3779b9u, frame)));
        if (slot >= 4u || params.slot_count[slot] == 0u) {
            return;
        }
        let e_idx = params.slot_start[slot]
            + (hash2(idx * 2654435769u, frame) % params.slot_count[slot]);
        let e = edges[min(e_idx, params.edge_count - 1u)];
        let n = mask_dir_to_world(e.normal);
        var life = mix(
            params.lifespan_min,
            params.lifespan_max,
            rand01(hash2(idx, frame ^ 2654435769u)),
        );
        // Ejecta roll: on onsets a fraction of spawns become shooting sparks
        // — fast along the normal, short-lived, streaked by the velocity-
        // stretch in render.wgsl.
        var speed = params.spawn_speed + params.burst_speed;
        if (rand01(hash2(idx ^ 0x51ed270bu, frame)) < params.ejecta_prob) {
            speed = params.ejecta_speed
                * (0.7 + 0.6 * rand01(hash2(idx, frame ^ 0x51ed270bu)));
            life = life * EJECTA_LIFE_MUL;
        }
        p.position = mask_uv_to_world(e.pos) + n * params.spawn_offset;
        p.velocity = n * speed;
        p.age = 0.0;
        p.lifespan = life;
        p.seed = rand01(hash2(idx, 2246822519u));
        p.slot = f32(slot);
        particles[idx] = p;
        return;
    }

    // --- Alive: forces -> drag -> integrate -> curl advection ----------
    p.age = p.age + params.dt;

    // Buoyancy: upward acceleration modulated by two incommensurate sines
    // along world x (plus a weak y term) — locally stronger columns of lift
    // form the licking tongues of a flame instead of a uniform rising sheet.
    // The sines each span ±1, so the sum spans ±1 after the 0.5 weights and
    // the multiplier stays positive for tongue_amp <= 1.
    let tongue = 1.0 + params.tongue_amp
        * (0.5 * sin(p.position.x * params.tongue_freq + params.time * 1.7)
            + 0.5 * sin(p.position.x * params.tongue_freq * 2.33
                - params.time * 1.1
                + p.position.y * params.tongue_freq * 0.5));
    var accel = vec2<f32>(0.0, params.buoyancy * tongue);

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
