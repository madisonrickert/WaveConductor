const vertexShader = require("./vertex.glsl");
const fragmentShader = require("./fragment.glsl");

export const PostShader = {
    uniforms: {
        time:      { value: 0 },
        tDiffuse:  { value: null },
    },
    vertexShader,
    fragmentShader,
};
