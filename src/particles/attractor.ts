import * as THREE from "three";

/**
 * 3D representation of an attractor in the line sketch.
 * It consists of a series of glowing, spinning rings.
 */
export class Attractor {
    static geometry = new THREE.RingGeometry(15, 18, 32);
    static materialSolid = new THREE.MeshBasicMaterial({
        side: THREE.DoubleSide,
        color: 0xC5E2CC,
        transparent: true,
        opacity: 0.6,
    });
    static NUM_RINGS = 10;

    ringMeshesGroup: THREE.Object3D = new THREE.Group();

    private _x: number = 0;
    private _y: number = 0;

    get x(): number {
        return this._x;
    }
    set x(value: number) {
        this._x = value;
        this.ringMeshesGroup.position.x = value;
    }

    get y(): number {
        return this._y;
    }
    set y(value: number) {
        this._y = value;
        this.ringMeshesGroup.position.y = value;
    }

    constructor(x = 0, y = 0, public power = 0) {
        // Set initial position
        this.x = x;
        this.y = y;
        this.ringMeshesGroup.name = "Attractor Rings";
        this.ringMeshesGroup.position.z = -100;

        this.ringMeshesGroup.rotation.x = 0.8; // Initial rotation
        for (let i = 0; i < Attractor.NUM_RINGS; i++) {
            const ring = new THREE.Mesh(Attractor.geometry, Attractor.materialSolid);
            const scale = 1 + Math.pow(i / 10, 2) * 2;
            ring.scale.set(scale, scale, scale);
            this.ringMeshesGroup.add(ring);
        }
        this.ringMeshesGroup.visible = false;
    }

    animate(_milliseconds: number) {
        if (this.power === 0) {
            if (this.ringMeshesGroup.visible) {
                this.ringMeshesGroup.visible = false;
            }
            return;
        }

        this.ringMeshesGroup.visible = true;

        // Rotate the rings
        const children = this.ringMeshesGroup.children;
        for (let idx = 0; idx < children.length; idx++) {
            const child = children[idx];
            child.rotation.y += (10 - idx) / 20 * this.power;
        }

        // Scale the rings based on power
        const scale = Math.sqrt(this.power) / 5;
        this.ringMeshesGroup.scale.set(scale, scale, scale);
    }
}
