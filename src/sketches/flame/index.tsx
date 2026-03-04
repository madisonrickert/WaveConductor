import React from "react";
import * as THREE from "three";
import { OrbitControls } from "three-stdlib";

import { createWhiteNoise, AudioNodeTracker } from "@/audio";
import { AFFINES, BoxCountVisitor, Branch, createInterpolatedVariation, createRouterVariation, LengthVarianceTrackerVisitor, SuperPoint, VARIATIONS, VelocityTrackerVisitor } from "./flame";
import { map } from "@/common/math";
import { loadSettings, saveSetting } from "@/common/sketchSettingsStore";
import { SettingDef } from "@/common/sketchSettings";
import { Sketch } from "@/common/sketch";
import { DEFAULT_NAME, FlameNameInput } from "./FlameNameInput";
import { FlamePointsMaterial } from "./flamePointsMaterial";
import { Chord } from "./types";
import { LeapHandController } from "@/common/leap/LeapHandController";

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

export default class FlameSketch extends Sketch {
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

    // Leap Motion
    private leapHands!: LeapHandController;

    // Grab-and-fling state
    private _grabbingHandCount = 0;
    private _lastGrabX = 0;
    private _lastGrabY = 0;
    private _grabMouseOffsetX = 0;
    private _grabMouseOffsetY = 0;
    private _angularVelocityX = 0;
    private _angularVelocityY = 0;

    // Audio nodes
    private audioTracker!: AudioNodeTracker;
    private noiseGain!: GainNode;
    private oscLow!: OscillatorNode;
    private oscHigh!: OscillatorNode;
    private oscHighGain!: GainNode;
    private oscGain!: GainNode;
    private chord!: Chord;
    private filter!: BiquadFilterNode;
    private compressor!: DynamicsCompressorNode;

    // Audio state
    private baseFrequency = 0;
    private baseLowFrequency = 0;
    private noiseGainScale = 0;
    private baseThirdBias = 0;
    private baseFifthBias = 0;
    private audioHasNoise = false;
    private audioHasChord = false;
    private oscLowGate = 0;
    private oscHighGate = 0;

    public render() {
        return <FlameNameInput key="input" initialName={this.savedName} onInput={(name, isEmpty) => this.updateName(name, isEmpty)} />;
    }

    public init() {
        this.initAudio();
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
        this.leapHands = new LeapHandController({
            canvas: this.canvas,
            renderer: this.renderer,
            getConnectionCallback: () => this.updateLeapConnectionCallback,
            getProtocolVersionCallback: () => this.updateLeapProtocolVersionCallback,
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

    public animate(_millisElapsed: number) {
        const currentTimeMs = performance.now();

        // Check for Leap Motion interaction
        if (this.leapHands.activeHandCount > 0) {
            this.markInteraction(currentTimeMs);
        }

        if (!this.isIdle) {
            this.animateSimulation();
        }

        this.updateIdleState(currentTimeMs);
    }

    private animateSimulation() {
        if (this.quality === "high") {
            this.animateSuperPoint();
        }

        const cameraLength = this.camera.position.length();
        this.compressor.ratio.setTargetAtTime(1 + 0.5 / (1. + cameraLength), this.audioContext.currentTime, 0.016);
        this.audioContext.gain.gain.setTargetAtTime((1.0 / (1. + cameraLength)) + 0.5, this.audioContext.currentTime, 0.016);

        this.material.setFocalLength(cameraLength);

        this.cDx = THREE.MathUtils.mapLinear(this.mousePosition.x, 0, this.canvas.width, -1, 1);
        this.cDy = THREE.MathUtils.mapLinear(this.mousePosition.y, 0, this.canvas.width, -1, 1);

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
        this.audioTracker.dispose();
        this.controls.dispose();
        this.geometry.dispose();
        this.material.dispose();
        this.scene.clear();

        // Clean up Leap Motion controller
        this.leapHands.dispose();
    }

    private animateSuperPoint() {
        const time = performance.now() / 3000;
        this.cX = 2 * sigmoid(6 * Math.sin(time)) - 1;
        const velocityVisitor = new VelocityTrackerVisitor();
        const varianceVisitor = new LengthVarianceTrackerVisitor();
        const countVisitor = new BoxCountVisitor([1, 0.1, 0.01, 0.001]);
        this.superPoint.recalculate(this.jumpiness, this.jumpiness, this.jumpiness, this.computeDepth(), true, velocityVisitor, varianceVisitor, countVisitor);

        this.updateAudio(velocityVisitor, varianceVisitor, countVisitor);
    }

    private updateAudio(
        velocityVisitor: VelocityTrackerVisitor,
        varianceVisitor: LengthVarianceTrackerVisitor,
        countVisitor: BoxCountVisitor,
    ) {
        const velocity = velocityVisitor.computeVelocity();
        const [count, countDensity] = countVisitor.computeCountAndCountDensity();

        // density ranges from 1 to ~6 or 7 at the high end.
        const density = countDensity / count;

        const velocityFactor = Math.min(velocity * this.noiseGainScale, 0.06);
        if (this.audioHasNoise) {
            const noiseAmplitude = 2 / (1 + density * density);
            const target = this.noiseGain.gain.value * 0.5 + 0.5 * (velocityFactor * noiseAmplitude + 1e-5);
            this.noiseGain.gain.setTargetAtTime(target, this.noiseGain.context.currentTime, 0.016);
        }

        const newOscGain = this.oscGain.gain.value * 0.9 + 0.1 * Math.max(0, Math.min(velocity * velocity * 2000, 0.6) - 0.01);
        this.oscGain.gain.setTargetAtTime(newOscGain, this.oscGain.context.currentTime, 0.016);

        if (this.audioHasChord) {
            const baseOffset = THREE.MathUtils.clamp(Math.floor(map(density, 1.0, 3, 0, 24)), 0, 48);
            this.chord.setScaleDegree(baseOffset);
            const target = (this.chord.gain.gain.value * 0.9 + 0.1 * (velocityFactor * count * count / 8 + 0.0001));
            this.chord.gain.gain.setTargetAtTime(target, this.chord.gain.context.currentTime, 0.016);
        } else {
            this.chord.gain.gain.setTargetAtTime(0, this.chord.gain.context.currentTime, 0.016);
        }
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
        this.baseFrequency = map((hash % 2048) / 2048, 0, 1, 10, 6000);
        const hash2 = hash * hash + hash * 31 + 9;
        this.filter.frequency.setValueAtTime(map((hash2 % 2e12) / 2e12, 0, 1, 120, 400), 0);
        const hash3 = hash2 * hash2 + hash2 * 31 + 9;
        this.filter.Q.setValueAtTime(map((hash3 % 2e12) / 2e12, 0, 1, 5, 8), 0);
        this.baseLowFrequency = map((hash3 % 10) / 10, 0, 1, 10, 20);
        this.noiseGainScale = map((hash2 * hash3 % 100) / 100, 0, 1, 0.5, 1);
        this.baseThirdBias = (hash2 % 4) / 4;
        this.baseFifthBias = (hash3 % 3) / 3;
        this.chord.setIsMajor(hash2 % 2 === 0);

        // basically boolean randoms; we don't want mod 2 cuz the hashes are related to each other at that small level
        this.audioHasNoise = (hash3 % 100) >= 50;
        this.oscLowGate = (hash2 * hash3 % 96) < 48 ? 0 : 1;
        this.oscHighGate = (hash3 * hash3 % 4000) < 2000 ? 0 : 1;
        this.audioHasChord = true;

        this.cY = map(hashNorm, 0, 1, -2.5, 2.5);

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
            this.superPoint.recalculate(this.jumpiness, this.jumpiness, this.jumpiness, this.computeDepth(), false);
        }
    }

    private initAudio() {
        const context = this.audioContext;
        const tracker = new AudioNodeTracker();
        this.audioTracker = tracker;

        this.compressor = context.createDynamicsCompressor();
        this.compressor.threshold.setValueAtTime(-40, 0);
        this.compressor.knee.setValueAtTime(35, 0);
        this.compressor.attack.setValueAtTime(0.1, 0);
        this.compressor.release.setValueAtTime(0.25, 0);
        this.compressor.ratio.setValueAtTime(1.8, 0);

        const noise = createWhiteNoise(context);
        tracker.trackSource(noise);
        this.noiseGain = context.createGain();
        this.noiseGain.gain.setValueAtTime(0, 0);
        noise.connect(this.noiseGain);
        this.noiseGain.connect(this.compressor);

        const { osc: oscLow, gain: oscLowGain } = tracker.createOsc(context, {
            frequency: 0,
            type: "square",
            gain: 0.6,
        });
        this.oscLow = oscLow;

        this.filter = context.createBiquadFilter();
        this.filter.type = "lowpass";
        this.filter.frequency.setValueAtTime(100, 0);
        this.filter.Q.setValueAtTime(2.18, 0);
        oscLowGain.connect(this.filter);

        const { osc: oscHigh, gain: oscHighGain } = tracker.createOsc(context, {
            frequency: 0,
            type: "triangle",
            gain: 0.05,
        });
        this.oscHigh = oscHigh;
        this.oscHighGain = oscHighGain;

        this.oscGain = context.createGain();
        this.oscGain.gain.setValueAtTime(0.0, 0);
        this.filter.connect(this.oscGain);
        this.oscHighGain.connect(this.oscGain);
        this.oscGain.connect(this.compressor);

        // plays a major or minor chord
        this.chord = (() => {
            const { osc: root } = tracker.createOsc(context, { type: "sine" });
            const { osc: third } = tracker.createOsc(context, { type: "sine" });
            const { osc: fifth, gain: fifthGain } = tracker.createOsc(context, { type: "sine", gain: 0.7 });
            const { osc: sub, gain: subGain } = tracker.createOsc(context, { type: "triangle", gain: 0.9 });
            const { osc: sub2, gain: sub2Gain } = tracker.createOsc(context, { type: "triangle", gain: 0.8 });

            const gain = context.createGain();
            gain.gain.setValueAtTime(0, 0);
            root.connect(gain);
            third.connect(gain);
            fifthGain.connect(gain);
            subGain.connect(gain);
            sub2Gain.connect(gain);

            tracker.trackNode(gain);

            // 0 = full major, 1 = full minor
            let minorBias = 0;
            const rootFreq = 120;
            let fifthBias = 0;
            let baseScaleDegree = 0;
            let isMajor = true;

            const MAJOR_SCALE = [0, 2, 4, 5, 7, 9, 11];
            const MINOR_SCALE = [0, 2, 3, 5, 7, 8, 10];

            function getSemitoneNumber(scaleIndex: number) {
                const scale = isMajor ? MAJOR_SCALE : MINOR_SCALE;
                const octave = Math.floor(scaleIndex / scale.length);
                const pitchClass = scaleIndex % scale.length;
                const semitoneNumber = octave * 12 + scale[pitchClass];
                return semitoneNumber;
            }

            function getFreq(semitoneNumber: number) {
                return rootFreq * Math.pow(2, semitoneNumber / 12);
            }

            function recompute() {
                const rootSemitone = getSemitoneNumber(baseScaleDegree + 0);
                root.frequency.setValueAtTime(getFreq(rootSemitone), 0);

                const thirdSemitone = getSemitoneNumber(baseScaleDegree + 3) - minorBias;
                third.frequency.setValueAtTime(getFreq(thirdSemitone), 0);

                const fifthSemitone = getSemitoneNumber(baseScaleDegree + 5) + fifthBias;
                fifth.frequency.setValueAtTime(getFreq(fifthSemitone), 0);

                sub.frequency.setValueAtTime(getFreq(rootSemitone) / 2, 0);
                sub2.frequency.setValueAtTime(getFreq(rootSemitone) / 4, 0);
            }

            return {
                root,
                third,
                fifth,
                gain,
                setIsMajor: (major: boolean) => {
                    isMajor = major;
                    recompute();
                },
                setScaleDegree: (sd: number) => {
                    baseScaleDegree = Math.round(sd);
                    recompute();
                },
                setMinorBias: (mB: number) => {
                    minorBias = Math.round(mB);
                    recompute();
                },
                setFifthBias: (fB: number) => {
                    fifthBias = Math.round(fB);
                    recompute();
                },
            };
        })();
        this.chord.gain.connect(this.compressor);

        tracker.trackNode(this.noiseGain, this.filter, this.oscGain, this.compressor);
        this.compressor.connect(context.gain);
    }

}
