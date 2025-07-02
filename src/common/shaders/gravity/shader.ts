import * as THREE from "three";
import vertexShader from "./vertex.glsl";
import fragmentShader from "./fragment.glsl";

export const gravityShader = {
    uniforms: {
        gamma:       { type: "f", value: 6.0 / 6.0 },
        iGlobalTime: { type: "f", value: 0 },
        iMouse:      { type: "v2", value: new THREE.Vector2(0, 0) },
        iMouseFactor: { type: "f", value: 1 / 15 },
        iResolution: { type: "v2", value: new THREE.Vector2(100, 100) },
        G:           { type: "f", value: 0 },
        tDiffuse:    { type: "t", value: null },
    },
    vertexShader,
    fragmentShader,
};
