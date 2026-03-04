import * as THREE from "three";

import { IParticle } from "./particleSystem";

export function createParticlePoints(particles: IParticle[], material: THREE.PointsMaterial) {
    const geometry = new THREE.BufferGeometry();
    const vertices: number[] = [];
    const colors: number[] = [];
    for (const particle of particles) {
        vertices.push(particle.x, particle.y, 0);
        colors.push(particle.color.x, particle.color.y, particle.color.z, particle.color.w);
    }
    geometry.setAttribute('position', new THREE.Float32BufferAttribute(vertices, 3));
    geometry.setAttribute('color', new THREE.Float32BufferAttribute(colors, 4));
    const pointCloud = new THREE.Points(geometry, material);
    return pointCloud;
}
