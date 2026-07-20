// Cymatics 2D wave-field simulation — one compute invocation per grid cell.
//
// Port of v4 `computeCellState.frag` (GPUComputationRenderer pass), with one
// deliberate deviation: source/alive-mask distances are measured in a
// height-normalized, aspect-corrected frame (see `main`) so the discs and the
// ripples they bound are CIRCLES in physical pixels at any window aspect. v4
// measured them in raw UV space, which stretched every disc to the window
// (horizontally elongated in landscape, vertically in portrait).
// Cell state is packed into an RGBA32F texel:
//   x = height            (surface displacement)
//   y = velocity          (rate of change of height)
//   z = accumulated_height (long-running integral, drives the render's glow)
//   w = unused            (carried through unchanged)
//
// Ping-pong model: the previous generation is read from `read_tex` via
// `textureLoad` (exact texel, no filtering) and the next generation is written
// to `write_tex` (write-only storage). The two textures alternate each sub-step
// (A->B, B->A) so a cell never reads and writes the same texel — which is what
// lets us avoid read_write storage (a downlevel feature we keep off the
// WebGPU-only target).
//
// Neighbour reads clamp to the edge: v4 sampled with a ClampToEdge wrap, so
// off-grid diagonal lookups resolve to the border texel.

struct SimParams {
    // Two wave-source centres in UV space [0,1]; both are always emitting. The
    // alive-mask and the per-source injection below are evaluated against the
    // nearer of the two.
    center: vec2<f32>,
    center2: vec2<f32>,
    // Sim grid size in texels (w, h). Drives UV<->texel conversion and bounds.
    resolution: vec2<u32>,
    // Radius of the active disc around the centres; outside it the field is
    // damped to zero by the alive-mask. Units: window heights (the
    // height-normalized distance frame in `main`), so the disc is a circle in
    // physical pixels — vertically it spans the same fraction of the window
    // as the old raw-UV radius did.
    active_radius: f32,
    // Scales the summed neighbour force (discrete Laplacian). v4 = 0.25.
    force_multiplier: f32,
    // Per-step velocity damping; < 1 bleeds energy so the field settles. v4 = 0.99818.
    velocity_decay: f32,
    // Per-step height damping. v4 = 0.9999.
    height_decay: f32,
    // Per-step decay of the accumulated-height integral. v4 = 0.999.
    accumulated_height_decay: f32,
    // Pads SimParams to a 16-byte multiple (mirrors SimParamsGpu::_pad).
    _pad: f32,
}

// Per-iteration phase. `time`, `wave_signal`, and `wave_signal2` are read; the
// buffer pads each slot to the 256-byte dynamic-offset stride (see
// IterParamsGpu). Field order is load-bearing: time @offset 0, wave_signal
// @offset 4, wave_signal2 @offset 8 — must match IterParamsGpu. `time` is the v4
// `iGlobalTime` advanced one sub-step. `wave_signal` / `wave_signal2` are the
// source values for centre 1 / centre 2, precomputed CPU-side (uniform across
// the dispatch, so hoisted out of the per-cell math below). In active play both
// hold the shared oscillator `amplitude·sin(time)` (one source); in the
// screensaver each holds its centre's independent raindrop envelope value.
struct IterParams {
    time: f32,
    wave_signal: f32,
    wave_signal2: f32,
}

@group(0) @binding(0) var<uniform> params: SimParams;
@group(0) @binding(1) var read_tex: texture_2d<f32>;
@group(0) @binding(2) var write_tex: texture_storage_2d<rgba32float, write>;
@group(0) @binding(3) var<uniform> iter: IterParams;

// v4 waveSourceAmount: the injection weight of a wave source at distance
// `dist`. Zero beyond two texels (the source is local), otherwise a soft
// Lorentzian falloff 1/(1 + (dist/texel)^2) clamped to [0,1]. `dist` and
// `texel_spacing` share the height-normalized frame built in `main`:
// `texel_spacing` is the length of one texel diagonal there (texels are
// square in that frame, so the injected source core is circular on screen).
fn wave_source_amount(dist: f32, texel_spacing: f32) -> f32 {
    if (dist >= texel_spacing * 2.0) { return 0.0; }
    return clamp(1.0 / (1.0 + pow(dist / texel_spacing, 2.0)), 0.0, 1.0);
}

// Edge-clamped texel fetch (ClampToEdge parity for off-grid neighbours).
fn load_clamped(coord: vec2<i32>, res: vec2<i32>) -> vec4<f32> {
    let c = clamp(coord, vec2<i32>(0, 0), res - vec2<i32>(1, 1));
    return textureLoad(read_tex, c, 0);
}

// v4 physicsForceContribution: one term of the discrete Laplacian — how much a
// neighbour's height pulls this cell toward it (neighbourHeight - height).
fn force_contribution(height: f32, coord: vec2<i32>, res: vec2<i32>) -> f32 {
    return load_clamped(coord, res).x - height;
}

// 8x8 tile per workgroup; the host dispatches ceil(resolution / 8) in each axis.
@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let res = params.resolution;
    // Bounds guard: the last workgroup row/column overhangs a non-multiple grid.
    if (gid.x >= res.x || gid.y >= res.y) { return; }

    let ires = vec2<i32>(i32(res.x), i32(res.y));
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    let resf = vec2<f32>(f32(res.x), f32(res.y));

    // Height-normalized, aspect-corrected distance frame. The grid has square
    // texels in window space (width = round(height · window_aspect), see
    // `derive_sim_grid`), so scaling the UV x-delta by the grid aspect makes
    // equal distances span equal PHYSICAL pixels in x and y: one unit = one
    // window height. Raw UV distances (v4) rendered every source disc as an
    // ellipse stretched to the window — horizontally elongated in landscape,
    // vertically in portrait.
    let aspect = resf.x / resf.y;
    // One texel's diagonal in the height-normalized frame: a texel is
    // (aspect/res.x, 1/res.y) = (~1/res.y, 1/res.y) there — square, so the
    // diagonal is sqrt(2)/res.y.
    let texel_spacing = length(vec2<f32>(1.0, 1.0) / resf.y);

    // Texel-centre UV (v4 used gl_FragCoord.xy = pixel + 0.5).
    let uv = (vec2<f32>(f32(gid.x), f32(gid.y)) + vec2<f32>(0.5)) / resf;

    // Distance to each source in the height-normalized frame; the field
    // reacts to whichever is nearer.
    let d1 = length((uv - params.center) * vec2<f32>(aspect, 1.0));
    let d2 = length((uv - params.center2) * vec2<f32>(aspect, 1.0));
    let min_dist = min(d1, d2);

    let cell = textureLoad(read_tex, coord, 0);
    var height = cell.x;
    var velocity = cell.y;
    var accumulated = cell.z;

    // v4 aliveAmount: 1 inside the active disc, fading to 0 at its edge. The
    // disc grows over the first ~500 units of time via the (iGlobalTime-500)/500
    // ramp, capped at +0.8, so the pattern blooms outward from the centres.
    let alive = clamp(
        params.active_radius + min(0.8, (iter.time - 500.0) / 500.0) - min_dist,
        0.0,
        1.0,
    );

    // v4 inactive early-out: a quiescent cell well outside the active disc keeps
    // its (near-zero) state. In the ping-pong model we must still WRITE it —
    // unlike v4's bare `return` (WebGL left the framebuffer texel untouched),
    // `write_tex` is a DIFFERENT texture from `read_tex`, so skipping the store
    // would leave the destination holding a stale value from two sub-steps ago.
    if (alive < 1e-3 && abs(height) < 1e-4 && abs(velocity) < 1e-4) {
        textureStore(write_tex, coord, cell);
        return;
    }

    // Discrete Laplacian over the 4 diagonal neighbours: the net height
    // differential is the spring force that propagates the wave.
    var force = 0.0;
    force += force_contribution(height, coord + vec2<i32>( 1,  1), ires);
    force += force_contribution(height, coord + vec2<i32>(-1,  1), ires);
    force += force_contribution(height, coord + vec2<i32>( 1, -1), ires);
    force += force_contribution(height, coord + vec2<i32>(-1, -1), ires);
    force *= params.force_multiplier;

    // Semi-implicit wave integration: force accelerates velocity, velocity
    // advances height; each is damped per step so energy slowly bleeds out.
    velocity += force;
    velocity *= params.velocity_decay;

    height += velocity;
    height *= params.height_decay;

    // Drive the two wave sources: blend height toward each source's own signal,
    // weighted by proximity to that centre (only the ~2-texel core). Each centre
    // carries its OWN precomputed signal: in active play `wave_signal` ==
    // `wave_signal2` (one shared oscillator), so this is identical to a single
    // shared source; in the screensaver each is an independent raindrop pulse, so
    // the two centres can ping asynchronously. Both are uniform across the
    // dispatch, so they are precomputed CPU-side rather than re-evaluated per cell.
    height = mix(height, iter.wave_signal, wave_source_amount(d1, texel_spacing));
    height = mix(height, iter.wave_signal2, wave_source_amount(d2, texel_spacing));

    // Mask everything outside the active disc back toward zero.
    height *= alive;
    velocity *= alive;

    // Leaky integral of height — the slowly-decaying ridge pattern the renderer
    // reads as the cymatic figure.
    accumulated *= params.accumulated_height_decay;
    accumulated += height;

    // Preserve the unused w channel (v4 carried cellState.w through unchanged).
    textureStore(write_tex, coord, vec4<f32>(height, velocity, accumulated, cell.w));
}
