import * as Leap from "leapjs";
import * as THREE from 'three';
import { mapLeapToThreePosition } from './util';
import lazy from "../lazy";

export type HandMesh = THREE.Object3D & {
    [childId: string]: THREE.Line | THREE.Mesh;
};

const boneGeometry = lazy(() => new THREE.SphereGeometry(10, 3, 3));
const boneMeshMaterial = lazy(() => new THREE.MeshBasicMaterial({
    color: 0xadd6b6,
    wireframeLinewidth: 5,
    wireframe: true,
}));

const boneLineMaterial = lazy(() => new THREE.LineBasicMaterial({
    color: 0xadd6b6,
    linewidth: 5,
}));

export function createHandMesh(): HandMesh {
    return new THREE.Object3D() as HandMesh;
}

export function updateHandMesh(handMesh: HandMesh, canvas: HTMLCanvasElement, hand: Leap.Hand) {
    hand.fingers.forEach((finger) => {
        if (handMesh["finger" + finger.type] == null) {
            const fingerLine = new THREE.Line(new THREE.BufferGeometry(), boneLineMaterial());
            handMesh["finger" + finger.type] = fingerLine;
            handMesh.add(fingerLine);
        }
        const fingerGeometry = handMesh["finger" + finger.type].geometry as THREE.BufferGeometry;
        finger.bones.forEach((bone) => {
            // create sphere for every bone
            const id = finger.type + ',' + bone.type;
            if (handMesh[id] == null) {
                const boneMesh = new THREE.Mesh(boneGeometry(), boneMeshMaterial());
                handMesh[id] = boneMesh;
                handMesh.add(boneMesh);
            }
            const position = mapLeapToThreePosition(canvas, bone.center());
            handMesh[id].position.copy(position);

            // create a line for every finger
            const positions = fingerGeometry.getAttribute('position') as THREE.Float32BufferAttribute || new THREE.Float32BufferAttribute(new Float32Array(bone.type * 3), 3);
            positions.setXYZ(bone.type, position.x, position.y, position.z);
            fingerGeometry.setAttribute('position', positions);
        });
    });
}