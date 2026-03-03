import * as THREE from "three";
import vertexShader from "./flamePoints.vert.frag";
import fragmentShader from "./flamePoints.frag";
import discUrl from "@/common/materials/starMaterial/disc.png";

export class FlamePointsMaterial extends THREE.ShaderMaterial {
    public map: THREE.Texture;

    private static uniforms = {
        focalLength: {
            value: 1.4,
        } as THREE.IUniform,
    };

    constructor() {
        super({
            vertexColors: true,
            transparent: true,
            opacity: 0.2,
            blending: THREE.AdditiveBlending,
            depthTest: false,
            uniforms: THREE.UniformsUtils.merge([
                THREE.UniformsLib.points,
                THREE.UniformsLib.fog,
                FlamePointsMaterial.uniforms,
            ]),
            vertexShader,
            fragmentShader,
        });
        const texture = new THREE.Texture();

        // trigger three's WebGLPrograms.getParameters() to recognize this has a texture
        this.map = texture;
        this.uniforms.map.value = texture;

        const loader = new THREE.ImageLoader(THREE.DefaultLoadingManager);
        loader.setCrossOrigin('Anonymous');
        loader.load(discUrl, (image) => {
            texture.image = image;
            texture.format = THREE.RGBAFormat;
            texture.needsUpdate = true;
        });
    }

    public setFocalLength(length: number) {
        this.uniforms.focalLength.value = length;
    }
}
