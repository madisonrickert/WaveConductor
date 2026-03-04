import * as THREE from "three";
import vertexShader from "./vertex.glsl";
import fragmentShader from "./fragment.glsl";

export const explodeShader = {
    uniforms: {
        iMouse:      { value: new THREE.Vector2(0, 0) },
        iResolution: { value: new THREE.Vector2(100, 100) },
        shrinkFactor: { value: 0.98 },
        gamma:       { value: 1.0 },
        tDiffuse:    { value: null },
    },
    vertexShader,
    fragmentShader,
};
