import * as Leap from "leapjs";
import * as THREE from 'three';
import { mapLeapToThreePosition } from './util';

// Consider cross-referencing with this plugin:
// https://github.com/leapmotion/leapjs-plugins/blob/master/main/bone-hand/leap.bone-hand.js

export class HandMesh extends THREE.Group {
    static boneGeometry = new THREE.SphereGeometry(10, 3, 3);
    static boneMeshMaterial = new THREE.MeshBasicMaterial({
        color: 0xadd6b6,
        wireframeLinewidth: 5,
        wireframe: true,
    });

    private bones: { [id: string]: THREE.Mesh } = {};

    constructor() {
        super();
        this.name = 'Hand';
    }

    private getBoneMesh(fingerType: number, boneType: number): THREE.Mesh {
        const id = `${fingerType},${boneType}`;
        if (!this.bones[id]) {
            const boneMesh = new THREE.Mesh(HandMesh.boneGeometry, HandMesh.boneMeshMaterial);
            this.bones[id] = boneMesh;
            this.add(boneMesh);
        }
        return this.bones[id];
    }

    private updateBonePosition(fingerType: number, boneType: number, x: number, y: number, z: number) {
        const id = `${fingerType},${boneType}`;
        if (this.bones[id]) {
            this.bones[id].position.set(x, y, z);
        }
    }

    public update(canvas: HTMLCanvasElement, hand: Leap.Hand) {
        for (const finger of hand.fingers) {
            for (const bone of finger.bones) {
                this.getBoneMesh(finger.type, bone.type);
                const {x, y} = mapLeapToThreePosition(canvas, bone.center());
                const z = 300; // z is set to 300 to keep it close to the camera
                this.updateBonePosition(finger.type, bone.type, x, y, z);
            }
        }
    }
}