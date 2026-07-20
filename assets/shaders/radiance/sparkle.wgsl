// Radiance extremity sparkles — a constellation of up to 12 small twinkling
// motes riding the dancers' high-motion extremities (fullscreen quad,
// fragment only; the default Material2d vertex shader supplies world
// position). Each mote is a soft gaussian core that breathes with its own
// twinkle waveform; a gentle four-point glint appears ONLY at the crest of
// the twinkle (smoothstep-gated), so the look is a living shimmer, not a
// static lens-flare cross. Motes are tinted with their body's identity color
// and their strength envelopes are CPU-eased — nothing pops. Additive
// (One, One) blending is set by RadianceSparkleMaterial::specialize; bloom
// supplies the halo.
//
// Bindings (group 2):
//   @binding(0): SparkleUniform —
//     sparkles[i]: xy = anchor world px, z = twinkle phase 0..1,
//                  w = strength (0 = off, CPU attack/release eased)
//     colors[i]:   rgb = linear-HDR body-identity tint,
//                  w = glint gain (how strongly the crest cross shows)
//     params:      x = master intensity, y = elapsed s,
//                  z = twinkle period s (highs shorten it), w reserved
//
// Struct parity: mirrors RadianceSparkleUniform in radiance/sparkle/mod.rs
// (MAX_SPARKLES = 12).

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

struct SparkleUniform {
    sparkles: array<vec4<f32>, 12>,
    colors: array<vec4<f32>, 12>,
    params: vec4<f32>,
};

@group(2) @binding(0) var<uniform> u: SparkleUniform;

const TAU: f32 = 6.2831853;
// Gaussian core radius, world px — small motes, not the old full-screen
// diffraction stars.
const CORE_SIGMA_PX: f32 = 7.0;
// Crest-glint spike reach (slow falloff axis), world px.
const SPIKE_LEN_PX: f32 = 34.0;
// Crest-glint spike thickness (fast falloff axis), world px.
const SPIKE_THIN_PX: f32 = 1.6;
// Twinkle level above which the four-point glint fades in (crest only).
const GLINT_ONSET: f32 = 0.72;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let p = in.world_position.xy;
    let t = u.params.y;
    let period = max(u.params.z, 1e-3);
    var rgb = vec3<f32>(0.0);
    for (var i = 0u; i < 12u; i = i + 1u) {
        let s = u.sparkles[i];
        if (s.w <= 0.001) {
            continue;
        }
        // Per-mote twinkle: shared period (highs-driven), staggered phases so
        // the constellation shimmers rather than blinking in unison.
        let twinkle = 0.5 + 0.5 * sin(TAU * (t / period + s.z));
        var d = p - s.xy;
        // Rotate odd motes' glint 45° so neighbouring crest-crosses stay
        // visually distinct.
        if ((i & 1u) == 1u) {
            let inv_sqrt2 = 0.70710678;
            d = vec2<f32>((d.x + d.y) * inv_sqrt2, (d.y - d.x) * inv_sqrt2);
        }
        let r2 = dot(d, d);
        // Soft gaussian core breathing with the twinkle (never fully dark —
        // the mote dims, it does not blink off).
        let core = exp(-r2 / (2.0 * CORE_SIGMA_PX * CORE_SIGMA_PX))
            * (0.35 + 0.65 * twinkle);
        // Crest glint: a gentle four-point cross, present only near the
        // twinkle peak and scaled by the CPU's glint gain (limb activity).
        let crest = smoothstep(GLINT_ONSET, 1.0, twinkle) * u.colors[i].w;
        let spike_x = exp(-abs(d.x) / SPIKE_LEN_PX - abs(d.y) / SPIKE_THIN_PX);
        let spike_y = exp(-abs(d.y) / SPIKE_LEN_PX - abs(d.x) / SPIKE_THIN_PX);
        let glint = (spike_x + spike_y) * crest * 0.8;
        rgb += u.colors[i].rgb * ((core + glint) * s.w);
    }
    // Additive (One, One): alpha is ignored by the blend.
    return vec4<f32>(rgb * u.params.x, 1.0);
}
