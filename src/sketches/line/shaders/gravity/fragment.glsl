uniform float iGlobalTime;
uniform vec2 iMouse;
uniform float iMouseFactor;
uniform vec2 iResolution;
uniform sampler2D tDiffuse;
varying vec2 vTextureCoord;
uniform float G;
uniform float gamma;

const float GRAVITY_EPSILON = 1e-4;

// Optimization to avoid calling
// `float intensityScalar = 0.8 / (i + 6. + sqrt(i+1));`
// each loop iteration
const int NUM_STEPS = 11;
const float INTENSITY_SCALARS[NUM_STEPS] = float[NUM_STEPS](
    0.114285714,
    0.095077216,
    0.082202612,
    0.072727273,
    0.065380480,
    0.059481810,
    0.054623350,
    0.050541977,
    0.047058824,
    0.044047339,
    0.041415103
);

vec2 gravity(vec2 p, vec2 attractionCenter, float g) {
    vec2 delta = attractionCenter - p;
    float distSq = max(dot(delta, delta), GRAVITY_EPSILON);
    return delta * (g / distSq);
}

vec4 equality(vec2 p, vec2 attractionCenter) {
    float total = 0.0;
    vec2 incomingP = p;
    vec2 outgoingP = p;
    vec4 c = vec4(0.0);
    vec4 outgoingColorFactor = vec4(0.96, 1.0, 1.0 / 0.96, 1.0);
    vec4 incomingColorFactor = vec4(1.0 / 0.96, 1.0, 0.96, 1.0);
    vec2 v_mousePull = (iMouse - p) * iMouseFactor;

    // Optimization for this code:
    // c += texture2D(tDiffuse, incomingP / iResolution) * intensityScalar * pow(incomingColorFactor, vec4(i));
    // c += texture2D(tDiffuse, outgoingP / iResolution) * intensityScalar * pow(outgoingColorFactor, vec4(i));
    // Rather than run `pow(___,vec4(i))` each iteration, just mutate and multiply these accumulators
    vec4 v_incomingAccum = incomingColorFactor;
    vec4 v_outgoingAccum = outgoingColorFactor;

    for(int i = 0; i < NUM_STEPS; i++) {
        incomingP = incomingP - gravity(incomingP, attractionCenter, G);
        outgoingP = outgoingP + gravity(outgoingP, attractionCenter, G);

        incomingP -= v_mousePull;
        outgoingP += v_mousePull;

        float intensityScalar = INTENSITY_SCALARS[i];
        vec2 v_incomingUV = incomingP / iResolution;
        vec2 v_outgoingUV = outgoingP / iResolution;

        c += texture2D(tDiffuse, v_incomingUV) * intensityScalar * v_incomingAccum;
        c += texture2D(tDiffuse, v_outgoingUV) * intensityScalar * v_outgoingAccum;

        // Mutate accumulators for i+1
        v_incomingAccum *= incomingColorFactor;
        v_outgoingAccum *= outgoingColorFactor;
    }
    return c;
}

void main(void) {
    vec2 uv = gl_FragCoord.xy;
    vec4 baseColor = texture2D(tDiffuse, vTextureCoord);
    vec4 gravityColor = equality(uv, iResolution / 2.0);
    gl_FragColor = pow(baseColor + gravityColor, vec4(gamma));
}