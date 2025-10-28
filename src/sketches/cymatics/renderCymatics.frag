
uniform vec2 cellStateResolution; // e.g. 1024 x 1024
#define cellOffset (1. / cellStateResolution)

uniform vec2 resolution;

// x = height
// y = velocity
// z = accumulated height
uniform sampler2D cellStateVariable;

uniform float skewIntensity;

const vec3 LIGHT_1_DIR = normalize(vec3(-1., -1., 0.3));
const vec3 LIGHT_2_DIR = normalize(vec3(-0.7, -1., 0.4));

const vec3 BASE_COL      = vec3(4.0, 32.0, 55.0) / 255.;
const vec3 BASE_BODY_COL = vec3(235.0, 89.0, 56.0) / 255.;
const vec3 LIGHT_1_COL   = vec3(254.0, 253.0, 255.0) / 255.;
const vec3 LIGHT_2_COL   = vec3(170.0, 89.0, 57.0) / 255.;

const float LIGHT_1_BRIGHTNESS = 0.6;
const float LIGHT_2_BRIGHTNESS = 0.3;

vec3 color(vec2 uv) {
    vec4 cellState = texture2D(cellStateVariable, uv);
    float height = cellState.x;
    float velocity = cellState.y;
    float accumulatedHeight = cellState.z;

    vec4 samplePlusX = texture2D(cellStateVariable, uv + vec2(cellOffset.x, 0.0));
    vec4 sampleMinusX = texture2D(cellStateVariable, uv - vec2(cellOffset.x, 0.0));
    vec4 samplePlusY = texture2D(cellStateVariable, uv + vec2(0.0, cellOffset.y));
    vec4 sampleMinusY = texture2D(cellStateVariable, uv - vec2(0.0, cellOffset.y));

    float halfTexelScaleX = 0.5 / cellOffset.x;
    float halfTexelScaleY = 0.5 / cellOffset.y;

    vec2 gradHeightAccX = (abs(samplePlusX.xz) - abs(sampleMinusX.xz)) * halfTexelScaleX;
    vec2 gradHeightAccY = (abs(samplePlusY.xz) - abs(sampleMinusY.xz)) * halfTexelScaleY;

    float gradHeightX = gradHeightAccX.x;
    float gradHeightY = gradHeightAccY.x;

    vec3 normal = normalize(vec3(gradHeightX, gradHeightY, 1.));
    

    float specular1 = max(0., dot(normal, LIGHT_1_DIR));
    // Apply power curve to highlights ^8, optimized version vs using pow()
    specular1 *= specular1;
    specular1 *= specular1;
    specular1 *= specular1;
    specular1 *= LIGHT_1_BRIGHTNESS;

    float specular2 = max(0., dot(normal, LIGHT_2_DIR));
    // Apply power curve to highlights ^8, optimized version vs using pow()
    specular2 *= specular2;
    specular2 *= specular2;
    specular2 *= specular2;
    specular2 *= LIGHT_2_BRIGHTNESS;
    
    float heightFactor = abs(height) * 3.;

    vec3 bodyColor = mix(BASE_BODY_COL, vec3(1.0), skewIntensity);
    vec3 col = mix(BASE_COL, bodyColor, heightFactor);
    col += specular1 * LIGHT_1_COL;
    col += specular2 * LIGHT_2_COL;
    return clamp(col, vec3(0.0), vec3(1.0));
}

float udRoundBox( vec2 uv, vec2 boxDimensions, float radius )
{
  return length(max(abs(uv)-boxDimensions,0.0))-radius;
}


void main() {
    vec2 screenCoord = gl_FragCoord.xy / resolution - vec2(0.5);
    vec2 normCoord;
    vec2 uv;

    vec3 col;
    float aspectRatio = resolution.x / resolution.y;
    vec3 cymaticsColor;
    if (aspectRatio > 1.0) {
        normCoord = screenCoord * vec2(aspectRatio, 1.);
        uv = normCoord + vec2(1.0, 0.5);
        cymaticsColor = color(uv);
    } else {
        normCoord = screenCoord * vec2(1., 1. / aspectRatio);
        uv = normCoord + vec2(0.5, 1.0);
        cymaticsColor = color(uv);
    }

    float vignetteAmount = 1. - clamp(-udRoundBox(screenCoord, vec2(0.45), 0.05) * 40., 0., 1.);
    vec3 colBg = vec3(0.25 - length(normCoord) * 0.2);
    col = mix(pow(cymaticsColor, vec3(mix(0.8, 1., vignetteAmount))), colBg, vignetteAmount);

    gl_FragColor = vec4(col, 1.0);
}

