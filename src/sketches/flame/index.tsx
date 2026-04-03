import React from "react";
import * as THREE from "three";
import { OrbitControls } from "three-stdlib";

import { Branch } from "./branch";
import { SuperPoint } from "./superPoint";
import { AFFINES, VARIATIONS, createInterpolatedVariation, createRouterVariation } from "./transforms";
import { BoxCountVisitor, VelocityTrackerVisitor } from "./updateVisitor";
import { map } from "@/math";
import { loadSettings, saveSetting } from "@/settings/store";
import { SettingDef } from "@/settings/types";
import { BaseSketch } from "@/sketch/BaseSketch";
import { DEFAULT_NAME, FlameNameInput } from "./FlameNameInput";
import { FlamePointsMaterial } from "./flamePointsMaterial";
import { FlameAudio } from "./audio";

import "./flame.scss";

const GEN_DIVISOR = 2147483648 - 1; // 2^31 - 1
const MAX_POINTS = 200000;

function randomBranches(
    name: string,
    getCX: () => number,
    getCY: () => number,
    getCDx: () => number,
    getCDy: () => number,
) {
    const numWraps = Math.floor(name.length / 5);
    const numBranches = Math.ceil(1 + name.length % 5 + numWraps);
    const branches: Branch[] = [];
    for (let i = 0; i < numBranches; i++) {
        const stringStart = map(i, 0, numBranches, 0, name.length);
        const stringEnd = map(i + 1, 0, numBranches, 0, name.length);
        const substring = name.substring(stringStart, stringEnd);
        branches.push(randomBranch(i, substring, numBranches, numWraps, getCX, getCY, getCDx, getCDy));
    }
    return branches;
}

// as low as 32 (for spaces)
// charCode - usually between 65 and 122
// other unicode languages could go up to 10k
function randomBranch(
    idx: number,
    substring: string,
    numBranches: number,
    numWraps: number,
    getCX: () => number,
    getCY: () => number,
    getCDx: () => number,
    getCDy: () => number,
) {
    let gen = stringHash(substring);
    function next() {
        return (gen = (gen * 4194303 + 127) % GEN_DIVISOR);
    }
    for (let i = 0; i < 5 + idx * numWraps; i++) {
        next();
    }
    const newVariation = () => {
        next();
        return objectValueByIndex(VARIATIONS, gen);
    };
    const random = () => {
        next();
        return gen / GEN_DIVISOR;
    };
    const affineBase = objectValueByIndex(AFFINES, gen);
    const affine = (point: THREE.Vector3) => {
        affineBase(point);
        point.x += getCX() / 5 + getCDx();
        point.y += getCY() / 5 + getCDy();
    };
    let variation = newVariation();

    if (random() < numWraps * 0.25) {
        variation = createInterpolatedVariation(
            variation,
            newVariation(),
            () => 0.5,
        );
    } else if (numWraps > 2 && random() < 0.2) {
        variation = createRouterVariation(
            variation,
            newVariation(),
            (p) => p.z < 0,
        );
    }
    const colorValues = [
        random() * 0.1 - 0.05,
        random() * 0.1 - 0.05,
        random() * 0.1 - 0.05,
    ];
    const focusIndex = idx % 3;
    colorValues[focusIndex] += 0.2;
    const color = new THREE.Color().fromArray(colorValues);
    color.multiplyScalar(numBranches / 3.5);
    const branch: Branch = {
        affine,
        color,
        variation,
    };
    return branch;
}

function objectValueByIndex<T>(obj: Record<string, T>, index: number) {
    const keys = Object.keys(obj);
    return obj[keys[index % keys.length]];
}

function stringHash(s: string) {
    let hash = 0, char;
    if (s.length === 0) { return hash; }
    for (let i = 0, l = s.length; i < l; i++) {
        char = s.charCodeAt(i);
        hash = hash * 31 + char;
        hash |= 0; // Convert to 32bit integer
    }
    hash *= hash * 31;
    return hash;
}

function sigmoid(x: number) {
    if (x > 10) {
        return 1;
    } else if (x < -10) {
        return 0;
    } else {
        return 1 / (1 + Math.exp(-x));
    }
}

export default class FlameSketch extends BaseSketch {
    static id = "flame";
    static settings = {
        name: { default: "", category: "user", label: "Name" } satisfies SettingDef<string>,
    };

    private quality = screen.width > 480 ? "high" : "low";
    private savedName: string = loadSettings("flame", FlameSketch.settings).name;
    public events = {
        dblclick: () => {
            this.markInteraction();
        },
        mousemove: (event: MouseEvent) => {
            const { x, y } = this.getRelativeCoordinates(event.clientX, event.clientY);
            this.mousePosition.x = x;
            this.mousePosition.y = y;
            this.markInteraction();
        },
        mousedown: (_event: MouseEvent) => {
            this.markInteraction();
        },
    };

    // Three.js
    private camera!: THREE.PerspectiveCamera;
    private scene!: THREE.Scene;
    private geometry!: THREE.BufferGeometry;
    private material = new FlamePointsMaterial();
    private pointCloud!: THREE.Points;
    private controls!: OrbitControls;

    // IFS state
    private globalBranches!: Branch[];
    private superPoint!: SuperPoint;
    private cX = 0;
    private cY = 0;
    private cDx = 0;
    private cDy = 0;
    private readonly jumpiness = 3;

    // Mouse
    private mousePosition = new THREE.Vector2(0, 0);

    // Grab-and-fling state
    private _grabbingHandCount = 0;
    private _lastGrabX = 0;
    private _lastGrabY = 0;
    private _grabMouseOffsetX = 0;
    private _grabMouseOffsetY = 0;
    private _angularVelocityX = 0;
    private _angularVelocityY = 0;

    // Audio
    private audio!: FlameAudio;

    // Reusable visitors (reset each frame to avoid per-frame allocations)
    private velocityVisitor = new VelocityTrackerVisitor();
    private countVisitor = new BoxCountVisitor([1, 0.1, 0.01, 0.001]);

    public render() {
        return <FlameNameInput key="input" initialName={this.savedName} onInput={(name, isEmpty) => this.updateName(name, isEmpty)} />;
    }

    public init() {
        this.audio = new FlameAudio(this.audioContext);
        const bgColor = new THREE.Color("#10101f");
        this.scene = new THREE.Scene();
        this.scene.fog = new THREE.Fog(bgColor.getHex(), 2, 60);
        this.scene.background = bgColor;

        this.camera = new THREE.PerspectiveCamera(60, 1 / this.aspectRatio, 0.01, 25);
        this.camera.position.z = 2;
        this.camera.position.y = 1;
        this.camera.lookAt(new THREE.Vector3());
        this.controls = new OrbitControls(this.camera, this.renderer.domElement);
        this.controls.autoRotate = true;
        this.controls.autoRotateSpeed = 1;
        this.controls.maxDistance = 8;
        this.controls.minDistance = 0.1;
        this.controls.enablePan = false;
        this.controls.enableDamping = true;
        this.controls.dampingFactor = 0.05;

        this.updateName(this.savedName || DEFAULT_NAME, !this.savedName);

        // Leap Motion setup
        this.leapHands = this.createLeapController({
            renderMode: { type: "overlay" },
            onFrame: (hands) => {
                // Only grabbing hands drive the sketch
                const grabbingHands = hands.filter(({ hand }) => hand.grabStrength > 0.5);

                if (grabbingHands.length === 0) {
                    if (this._grabbingHandCount > 0) {
                        this._grabbingHandCount = 0;
                    }
                    return;
                }

                // Average positions of grabbing hands only
                let avgX = 0;
                let avgY = 0;
                for (const { canvasPosition } of grabbingHands) {
                    avgX += canvasPosition.x;
                    avgY += canvasPosition.y;
                }
                avgX /= grabbingHands.length;
                avgY /= grabbingHands.length;

                if (grabbingHands.length !== this._grabbingHandCount) {
                    this._grabMouseOffsetX = this.mousePosition.x - avgX;
                    this._grabMouseOffsetY = this.mousePosition.y - avgY;
                    this._lastGrabX = avgX;
                    this._lastGrabY = avgY;
                    if (this._grabbingHandCount === 0) {
                        this._angularVelocityX = 0;
                        this._angularVelocityY = 0;
                    }
                    this._grabbingHandCount = grabbingHands.length;
                } else {
                    const deltaX = (avgX - this._lastGrabX) / this.canvas.width * Math.PI * 2;
                    const deltaY = (avgY - this._lastGrabY) / this.canvas.height * Math.PI * 2;
                    this.controls.setAzimuthalAngle(this.controls.getAzimuthalAngle() - deltaX);
                    this.controls.setPolarAngle(this.controls.getPolarAngle() - deltaY);
                    this._angularVelocityX = this._angularVelocityX * 0.7 + deltaX * 0.3;
                    this._angularVelocityY = this._angularVelocityY * 0.7 + deltaY * 0.3;
                    this._lastGrabX = avgX;
                    this._lastGrabY = avgY;
                }

                this.mousePosition.set(avgX + this._grabMouseOffsetX, avgY + this._grabMouseOffsetY);
            },
        });
    }

    protected step() {
        if (this.quality === "high") {
            this.animateSuperPoint();
        }

        const cameraLength = this.camera.position.length();
        this.audio.updateForCamera(cameraLength);

        this.material.setFocalLength(cameraLength);

        this.cDx = THREE.MathUtils.mapLinear(this.mousePosition.x, 0, this.canvas.width, -1, 1);
        this.cDy = THREE.MathUtils.mapLinear(this.mousePosition.y, 0, this.canvas.height, -1, 1);

        // Apply fling momentum from Leap grab release
        if (this._grabbingHandCount === 0 && (Math.abs(this._angularVelocityX) > 0.0001 || Math.abs(this._angularVelocityY) > 0.0001)) {
            this.controls.setAzimuthalAngle(this.controls.getAzimuthalAngle() - this._angularVelocityX);
            this.controls.setPolarAngle(this.controls.getPolarAngle() - this._angularVelocityY);
            this._angularVelocityX *= 0.95;
            this._angularVelocityY *= 0.95;
        }

        this.controls.update();
        this.renderer.render(this.scene, this.camera);

        // Render hand meshes on top
        this.leapHands.renderOverlay();
    }

    public resize(width: number, height: number) {
        this.camera.aspect = width / height;
        this.camera.updateProjectionMatrix();
        this.leapHands?.resize(width, height);
    }

    public destroy() {
        super.destroy();
        this.audio.dispose();
        this.controls.dispose();
        this.geometry.dispose();
        this.material.dispose();
        this.scene.clear();
    }

    private animateSuperPoint() {
        const time = performance.now() / 3000;
        this.cX = 2 * sigmoid(6 * Math.sin(time)) - 1;
        this.velocityVisitor.reset();
        this.countVisitor.reset();
        this.superPoint.recalculate(this.jumpiness, this.jumpiness, this.jumpiness, this.computeDepth(), true, [this.velocityVisitor, this.countVisitor]);

        const velocity = this.velocityVisitor.computeVelocity();
        const [count, countDensity] = this.countVisitor.computeCountAndCountDensity();
        this.audio.updateFromFractalStats(velocity, count, countDensity);
    }

    private computeDepth() {
        // points at exactly depth d = b^d
        // points from depth 0...d = b^0 + b^1 + b^2 + ... b^d
        // we want total points to be ~120k, so only the last level really matters
        const depth = (this.globalBranches.length === 1)
            ? 1000
            : Math.floor(Math.log(100000) / Math.log(this.globalBranches.length));
        return depth;
    }

    public updateName(name: string = DEFAULT_NAME, isEmpty: boolean = true) {
        this.audioContext.gain.gain.setValueAtTime(0, 0);
        saveSetting("flame", FlameSketch.settings, "name", isEmpty ? "" : name);

        const hash = stringHash(name);
        const hashNorm = (hash % 1024) / 1024;
        const hash2 = hash * hash + hash * 31 + 9;
        const hash3 = hash2 * hash2 + hash2 * 31 + 9;
        this.audio.configureForName(hash, hash2, hash3);

        this.cY = map(hashNorm, 0, 1, -2.5, 2.5);

        // Dispose old geometry and point cloud before creating new ones
        this.geometry?.dispose();

        // Reset slot counter before building a new tree
        SuperPoint.nextSlot = 0;
        this.globalBranches = randomBranches(
            name,
            () => this.cX,
            () => this.cY,
            () => this.cDx,
            () => this.cDy,
        );

        // Pre-allocate geometry with MAX_POINTS capacity
        this.geometry = new THREE.BufferGeometry();
        this.geometry.setAttribute('position', new THREE.Float32BufferAttribute(new Float32Array(MAX_POINTS * 3), 3));
        this.geometry.setAttribute('color', new THREE.Float32BufferAttribute(new Float32Array(MAX_POINTS * 3), 3));
        this.geometry.setDrawRange(0, 0);

        this.superPoint = new SuperPoint(
            new THREE.Vector3(0, 0, 0),
            new THREE.Color(0, 0, 0),
            this.geometry,
            this.globalBranches,
        );

        this.scene.remove(this.pointCloud);

        this.pointCloud = new THREE.Points(this.geometry, this.material);
        this.pointCloud.rotateX(-Math.PI / 2);
        this.scene.add(this.pointCloud);

        if (this.quality === "low") {
            this.superPoint.recalculate(this.jumpiness, this.jumpiness, this.jumpiness, this.computeDepth(), false, []);
        }
    }

}
