import * as THREE from "three";

import { IParticle } from "./particleSystem";

export function createParticlePoints(particles: IParticle[], material: THREE.PointsMaterial) {
    const geometry = new THREE.BufferGeometry();
    const vertices: number[] = [];
    const colors: number[] = [];
    for (const particle of particles) {
        const vertex = new THREE.Vector3(particle.x, particle.y, 0);
        particle.vertex = vertex;
        vertices.push(vertex.x, vertex.y, vertex.z);
        const color = new THREE.Vector4(1,1,1,0);
        particle.color = color;
        colors.push(color.x, color.y, color.z, color.w);
    }
    geometry.setAttribute('position', new THREE.Float32BufferAttribute(vertices, 3));
    geometry.setAttribute('color', new THREE.Float32BufferAttribute(colors, 4));
    const pointCloud = new THREE.Points(geometry, material);
    return pointCloud;
}
