// Radiance extremity sparkles — two star-glints of additive HDR light
// anchored to the dancer's fastest-oscillating limb and its contralateral
// partner (fullscreen quad, fragment only; the default Material2d vertex
// shader supplies world position). Each glint is a soft gaussian core plus
// a four-point diffraction cross; the mirror sparkle's animation clock lags
// by the configured offset and its cross is rotated 45° so the pair reads
// as two distinct stars. Additive (One, One) blending is set by
// RadianceSparkleMaterial::specialize; bloom supplies the halo.
//
// Bindings (group 2):
//   @binding(0): SparkleUniform —
//     sparkles[i]: xy = anchor world px, z = animation clock offset s,
//                  w = strength (0 = off)
//     colors[i]:   rgb = linear-HDR rainbow color (CPU-cycled, 7 s wheel)
//     params:      x = master intensity, y = elapsed s, z = twinkle
//                  period s, w reserved
//
// Struct parity: mirrors RadianceSparkleUniform in radiance/sparkle.rs.

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

struct SparkleUniform {
    sparkles: array<vec4<f32>, 2>,
    colors: array<vec4<f32>, 2>,
    params: vec4<f32>,
};

@group(2) @binding(0) var<uniform> u: SparkleUniform;

const TAU: f32 = 6.2831853;
// Gaussian core radius, world px.
const CORE_SIGMA_PX: f32 = 14.0;
// Diffraction-spike reach (slow falloff axis), world px.
const SPIKE_LEN_PX: f32 = 80.0;
// Diffraction-spike thickness (fast falloff axis), world px.
const SPIKE_THIN_PX: f32 = 3.0;

// One star-glint: core + four-point cross, both breathing with the twinkle.
fn glint(d: vec2<f32>, twinkle: f32) -> f32 {
    let r2 = dot(d, d);
    let core = exp(-r2 / (2.0 * CORE_SIGMA_PX * CORE_SIGMA_PX));
    let spike_x = exp(-abs(d.x) / SPIKE_LEN_PX - abs(d.y) / SPIKE_THIN_PX);
    let spike_y = exp(-abs(d.y) / SPIKE_LEN_PX - abs(d.x) / SPIKE_THIN_PX);
    // The cross rides the twinkle harder than the core, so the star visibly
    // flares and relaxes instead of merely dimming.
    return core * (0.65 + 0.6 * twinkle) + (spike_x + spike_y) * (0.25 + 0.95 * twinkle);
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let p = in.world_position.xy;
    var rgb = vec3<f32>(0.0);
    for (var i = 0u; i < 2u; i = i + 1u) {
        let s = u.sparkles[i];
        if (s.w <= 0.0) {
            continue;
        }
        // Per-sparkle animation clock: the mirror lags by its offset (0.5 s),
        // giving the pair the specified out-of-step twinkle.
        let t = u.params.y - s.z;
        let twinkle = 0.5 + 0.5 * sin(TAU * t / u.params.z);
        var d = p - s.xy;
        if (i == 1u) {
            // Rotate the mirror's cross 45° so the two stars are visually
            // distinct even when their limbs cross.
            let inv_sqrt2 = 0.70710678;
            d = vec2<f32>((d.x + d.y) * inv_sqrt2, (d.y - d.x) * inv_sqrt2);
        }
        rgb += u.colors[i].rgb * (glint(d, twinkle) * s.w);
    }
    // Additive (One, One): alpha is ignored by the blend.
    return vec4<f32>(rgb * u.params.x, 1.0);
}
