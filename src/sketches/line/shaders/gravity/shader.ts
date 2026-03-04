import * as THREE from "three";
import vertexShader from "./vertex.glsl";
import fragmentShader from "./fragment.glsl";

export const gravityShader = {
    uniforms: {
        gamma:       { value: 6.0 / 6.0 },
        iGlobalTime: { value: 0 },
        iMouse:      { value: new THREE.Vector2(0, 0) },
        iMouseFactor: { value: 1 / 15 },
        iResolution: { value: new THREE.Vector2(100, 100) },
        G:           { value: 0 },
        tDiffuse:    { value: null },
    },
    vertexShader,
    fragmentShader,
};
