import * as Leap from "leapjs";
import * as THREE from 'three';
import { mapLeapToThreePosition } from './util';

// Consider cross-referencing with this plugin:
// https://github.com/leapmotion/leapjs-plugins/blob/master/main/bone-hand/leap.bone-hand.js

export class HandMesh extends THREE.Group {
    static boneGeometry = new THREE.SphereGeometry(10, 3, 3);
    static defaultMaterial = new THREE.MeshBasicMaterial({
        color: 0xadd6b6,
        wireframeLinewidth: 5,
        wireframe: true,
    });

    private bones: { [id: string]: THREE.Mesh } = {};
    private material: THREE.MeshBasicMaterial;

    constructor(material?: THREE.MeshBasicMaterial) {
        super();
        this.name = 'Hand';
        this.material = material ?? HandMesh.defaultMaterial;
    }

    private getBoneMesh(fingerType: number, boneType: number): THREE.Mesh {
        const id = `${fingerType},${boneType}`;
        if (!this.bones[id]) {
            const boneMesh = new THREE.Mesh(HandMesh.boneGeometry, this.material);
            this.bones[id] = boneMesh;
            this.add(boneMesh);
        }
        return this.bones[id];
    }

    public update(canvas: HTMLCanvasElement, hand: Leap.Hand) {
        for (const finger of hand.fingers) {
            for (const bone of finger.bones) {
                const mesh = this.getBoneMesh(finger.type, bone.type);
                const {x, y} = mapLeapToThreePosition(canvas, bone.center());
                mesh.position.set(x, y, 300);
            }
        }
    }
}