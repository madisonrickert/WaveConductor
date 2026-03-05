// Comes from GPUComputationRenderer
// #define resolution vec2( 1024.0, 1024.0 )

// Comes from adding cellStateVariable as a variable dependency in GPUComputationRenderer
// uniform sampler2D cellStateVarible;
// x = height
// y = velocity
// z = accumulated height

// additionally, we get everything from Three's WebGLProgram: https://threejs.org/docs/#api/renderers/webgl/WebGLProgram

uniform float iGlobalTime;
// uniform vec2 iMouse;
uniform vec2 center;
uniform vec2 center2;
uniform float activeRadius;

const float FORCE_MULTIPLIER = 0.25; 
const float VELOCITY_DECAY_FACTOR = 0.99818;
const float HEIGHT_DECAY_FACTOR = 0.9999;
const float ACCUMULATED_HEIGHT_DECAY_FACTOR = 0.999;

const vec2 v_texelSize = (1. / resolution);
const float f_texelSpacing = length(v_texelSize);
const vec2 v_texelDiagUR = vec2(+v_texelSize.x, +v_texelSize.y);
const vec2 v_texelDiagUL = vec2(-v_texelSize.x, +v_texelSize.y);
const vec2 v_texelDiagLR = vec2(+v_texelSize.x, -v_texelSize.y);
const vec2 v_texelDiagLL = vec2(-v_texelSize.x, -v_texelSize.y);

float waveSourceAmount(float dist) {
    if (dist >= f_texelSpacing * 2.) return 0.;
    return clamp(1. / (1. + pow(dist / f_texelSpacing, 2.)), 0., 1.);
}

float physicsForceContribution(float height, vec2 coord) {
    vec4 neighborState = texture2D(cellStateVariable, coord);
    float neighborHeight = neighborState.x;

    return (neighborHeight - height);
}

void main() {
    vec2 v_uv = gl_FragCoord.xy / resolution;
    
    vec2 v_uvOffsetFromCenter = v_uv - center;
    float uvOffsetFromCenterLength = length(v_uvOffsetFromCenter);

    vec2 v_uvOffsetFromCenter2 = v_uv - center2;
    float uvOffsetFromCenter2Length = length(v_uvOffsetFromCenter2);

    // Use nearest center distance — both centers always active
    float minDistFromCenter = min(uvOffsetFromCenterLength, uvOffsetFromCenter2Length);

    vec4 cellState = texture2D(cellStateVariable, v_uv);
    float height = cellState.x;
    float velocity = cellState.y;
    float accumulatedHeight = cellState.z;

    float aliveAmount = clamp(activeRadius + min(0.8, (iGlobalTime - 500.) / 500.) - minDistFromCenter, 0., 1.);

    // Exit early for inactive cells
    if (aliveAmount < 1e-3 && abs(height) < 1e-4 && abs (velocity) < 1e-4) {
        return;
    }

    float force = 0.;
    force += physicsForceContribution(height, v_uv + v_texelDiagUR);
    force += physicsForceContribution(height, v_uv + v_texelDiagUL);
    force += physicsForceContribution(height, v_uv + v_texelDiagLR);
    force += physicsForceContribution(height, v_uv + v_texelDiagLL);
    force *= FORCE_MULTIPLIER;

    velocity += force;
    velocity *= VELOCITY_DECAY_FACTOR;

    height += velocity;
    height *= HEIGHT_DECAY_FACTOR;

    float waveSignal = 2. * sin(iGlobalTime);
    height = mix(height, waveSignal, waveSourceAmount(uvOffsetFromCenterLength));
    height = mix(height, waveSignal, waveSourceAmount(uvOffsetFromCenter2Length));

    height *= aliveAmount;
    velocity *= aliveAmount;

    accumulatedHeight *= ACCUMULATED_HEIGHT_DECAY_FACTOR;
    accumulatedHeight += height;

    vec4 newCellState = vec4(height, velocity, accumulatedHeight, cellState.w);
    gl_FragColor = newCellState;
}
