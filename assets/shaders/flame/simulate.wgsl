// Flame IFS: one dispatch per tree level; each thread computes one node from
// its parent (updated by the previous dispatch — WebGPU guarantees storage
// visibility between dispatches in a pass).
//
// Kernel parity: this file mirrors crates/wc-sketches/src/flame/branches.rs
// (apply_variation_cpu / apply_branch_cpu) term for term. Change both together.

struct FlameNode {
    pos: vec3<f32>,
    _pad0: f32,
    color: vec3<f32>,
    _pad1: f32,
}

struct Branch {
    mat_x: vec4<f32>,
    mat_y: vec4<f32>,
    mat_z: vec4<f32>,
    offset: vec4<f32>,
    color: vec4<f32>,
    var_a: u32,
    var_b: u32,
    mode: u32,
    _pad: u32,
}

struct SimParams {
    branches: array<Branch, 8>,
    warp: vec2<f32>,
    lerp_pos: f32,
    lerp_col: f32,
    branch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

struct LevelParams {
    level_start: u32,
    node_count: u32,
    parent_start: u32,
    parent_count: u32,
}

@group(0) @binding(0) var<uniform> sim: SimParams;
@group(0) @binding(1) var<storage, read_write> nodes: array<FlameNode>;
@group(0) @binding(2) var<uniform> level: LevelParams;

const PI: f32 = 3.14159265358979;
// v4: points escaping |p| > 50 are pulled back with the Spherical variation.
const ESCAPE_RADIUS_SQ: f32 = 2500.0;

// The seven v4 variations (transforms.ts::VARIATIONS), zero-safe like
// THREE.js (normalize/setLength divide by length || 1).
fn apply_variation(id: u32, p: vec3<f32>) -> vec3<f32> {
    let len_sq = dot(p, p);
    switch id {
        case 0u: { return p; }                                   // Linear
        case 1u: { return sin(p); }                              // Sin
        case 2u: {                                               // Spherical
            if (len_sq == 0.0) { return p; }
            return p / len_sq;
        }
        case 3u: {                                               // Polar
            return vec3<f32>(atan2(p.y, p.x) / PI, sqrt(len_sq) - 1.0, atan2(p.z, p.x));
        }
        case 4u: {                                               // Swirl
            let s = sin(len_sq);
            let c = cos(len_sq);
            return vec3<f32>(p.z * s - p.y * c, p.x * c + p.z * s, p.x * s - p.y * s);
        }
        case 5u: {                                               // Normalize
            if (len_sq == 0.0) { return p; }
            return p / sqrt(len_sq);
        }
        default: {                                               // Shrink
            if (len_sq == 0.0) { return p; }
            return p * (exp(-len_sq) / sqrt(len_sq));
        }
    }
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let local = gid.x;
    if (local >= level.node_count) {
        return;
    }
    // Branch-major layout: contiguous runs share a branch (warp-coherent switch).
    let branch_idx = local / level.parent_count;
    let parent_idx = level.parent_start + (local % level.parent_count);
    let node_idx = level.level_start + local;

    let b = sim.branches[branch_idx];
    let parent = nodes[parent_idx];

    // Affine (row-major mat * p + offset), then the per-frame warp on x/y,
    // matching v4's randomBranch closure order (warp BEFORE the variation).
    // `pt` = target point; `target` is a reserved WGSL keyword.
    var pt = vec3<f32>(
        dot(b.mat_x.xyz, parent.pos),
        dot(b.mat_y.xyz, parent.pos),
        dot(b.mat_z.xyz, parent.pos),
    ) + b.offset.xyz;
    pt = vec3<f32>(pt.x + sim.warp.x, pt.y + sim.warp.y, pt.z);

    // Variation combinator: single / interpolated(0.5) / router(z < 0).
    switch b.mode {
        case 0u: { pt = apply_variation(b.var_a, pt); }
        case 1u: {
            pt = mix(apply_variation(b.var_a, pt), apply_variation(b.var_b, pt), 0.5);
        }
        default: {
            if (pt.z < 0.0) {
                pt = apply_variation(b.var_a, pt);
            } else {
                pt = apply_variation(b.var_b, pt);
            }
        }
    }

    // Per-frame settle: lerp toward the target (v4: 0.8 pos / 0.75 color).
    let node = nodes[node_idx];
    var new_pos = mix(node.pos, pt, sim.lerp_pos);
    // Escape pullback (v4: Spherical when |p|^2 > 2500).
    let esc_sq = dot(new_pos, new_pos);
    if (esc_sq > ESCAPE_RADIUS_SQ) {
        new_pos = new_pos / esc_sq;
    }
    let target_col = parent.color + b.color.rgb;
    let new_col = mix(node.color, target_col, sim.lerp_col);

    nodes[node_idx].pos = new_pos;
    nodes[node_idx].color = new_col;
}
