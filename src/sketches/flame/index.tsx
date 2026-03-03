import React from "react";
import * as THREE from "three";
import { OrbitControls } from "three-stdlib";

import { createWhiteNoise } from "@/audio/noise";
import { AFFINES, BoxCountVisitor, Branch, createInterpolatedVariation, createRouterVariation, LengthVarianceTrackerVisitor, SuperPoint, VARIATIONS, VelocityTrackerVisitor } from "@/common/flame";
import { map } from "@/common/math";
import { getQueryParam, setQueryParams } from "@/common/queryParams";
import { ISketch } from "@/sketch";
import { FlamePointsMaterial } from "./flamePointsMaterial";
import { Chord } from "./types";

import "./flame.scss";

const quality = screen.width > 480 ? "high" : "low";
const GEN_DIVISOR = 2147483648 - 1; // 2^31 - 1
const MAX_POINTS = 200000;

const nameFromSearch: string = getQueryParam("name");

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

class FlameNameInput extends React.Component<{ onInput: (newName: string) => void }, object> {
    public render() {
        return (
            <div className="flame-input">
                <input
                    defaultValue={nameFromSearch}
                    placeholder="Han"
                    maxLength={20}
                    onInput={this.handleInput}
                />
            </div>
        );
    }

    private handleInput = (event: React.FormEvent<HTMLInputElement>) => {
        const value = event.currentTarget.value;
        const name = (value == null || value === "") ? "Han" : value.trim();
        this.props.onInput(name);
    }
}

export default class FlameSketch extends ISketch {
    public elements = [<FlameNameInput key="input" onInput={(name) => this.updateName(name)} />];
    public id = "flame";
    public events = {
        dblclick: () => { },
        mousemove: (event: MouseEvent) => {
            const { x, y } = this.getRelativeCoordinates(event.clientX, event.clientY);
            this.mousePosition.x = x;
            this.mousePosition.y = y;
        },
        mousedown: (_event: MouseEvent) => { },
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

    // Audio nodes
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

        this.updateName(nameFromSearch);
    }

    public animate(_millisElapsed: number) {
        if (quality === "high") {
            this.animateSuperPoint();
        }

        const cameraLength = this.camera.position.length();
        this.compressor.ratio.setTargetAtTime(1 + 0.5 / (1. + cameraLength), this.audioContext.currentTime, 0.016);
        this.audioContext.gain.gain.setTargetAtTime((1.0 / (1. + cameraLength)) + 0.5, this.audioContext.currentTime, 0.016);

        this.material.setFocalLength(cameraLength);

        this.cDx = THREE.MathUtils.mapLinear(this.mousePosition.x, 0, this.canvas.width, -1, 1);
        this.cDy = THREE.MathUtils.mapLinear(this.mousePosition.y, 0, this.canvas.width, -1, 1);

        this.controls.update();
        this.renderer.render(this.scene, this.camera);
    }

    public resize(width: number, height: number) {
        this.camera.aspect = width / height;
        this.camera.updateProjectionMatrix();
    }

    public destroy() {
        this.oscLow.stop();
        this.oscHigh.stop();
        this.chord.root.stop();
        this.chord.third.stop();
        this.chord.fifth.stop();
        this.controls.dispose();
        this.geometry.dispose();
        this.material.dispose();
        this.scene.clear();
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
        // const variance = varianceVisitor.computeVariance();
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

    private getRelativeCoordinates(clientX: number, clientY: number) {
        const rect = this.canvas.getBoundingClientRect();
        return {
            x: clientX - rect.left,
            y: clientY - rect.top,
        };
    }

    public updateName(name: string = "Han") {
        this.audioContext.gain.gain.setValueAtTime(0, 0);
        setQueryParams({ name });

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

        if (quality === "low") {
            this.superPoint.recalculate(this.jumpiness, this.jumpiness, this.jumpiness, this.computeDepth(), false);
        }
    }

    private initAudio() {
        const context = this.audioContext;

        this.compressor = context.createDynamicsCompressor();
        this.compressor.threshold.setValueAtTime(-40, 0);
        this.compressor.knee.setValueAtTime(35, 0);
        this.compressor.attack.setValueAtTime(0.1, 0);
        this.compressor.release.setValueAtTime(0.25, 0);
        this.compressor.ratio.setValueAtTime(1.8, 0);

        // const noise = createPinkNoise(context);
        const noise = createWhiteNoise(context);
        this.noiseGain = context.createGain();
        this.noiseGain.gain.setValueAtTime(0, 0);
        noise.connect(this.noiseGain);
        this.noiseGain.connect(this.compressor);

        this.oscLow = context.createOscillator();
        this.oscLow.frequency.setValueAtTime(0, 0);
        this.oscLow.type = "square";
        this.oscLow.start(0);
        const oscLowGain = context.createGain();
        oscLowGain.gain.setValueAtTime(0.6, 0);
        this.oscLow.connect(oscLowGain);

        this.filter = context.createBiquadFilter();
        this.filter.type = "lowpass";
        this.filter.frequency.setValueAtTime(100, 0);
        this.filter.Q.setValueAtTime(2.18, 0);
        oscLowGain.connect(this.filter);

        this.oscHigh = context.createOscillator();
        this.oscHigh.frequency.setValueAtTime(0, 0);
        this.oscHigh.type = "triangle";
        this.oscHigh.start(0);
        this.oscHighGain = context.createGain();
        this.oscHighGain.gain.setValueAtTime(0.05, 0);
        this.oscHigh.connect(this.oscHighGain);

        this.oscGain = context.createGain();
        this.oscGain.gain.setValueAtTime(0.0, 0);
        this.filter.connect(this.oscGain);
        this.oscHighGain.connect(this.oscGain);
        this.oscGain.connect(this.compressor);

        // plays a major or minor chord
        this.chord = (() => {
            const root = context.createOscillator();
            root.type = "sine";
            root.start(0);

            const third = context.createOscillator();
            third.type = "sine";
            third.start(0);

            const fifth = context.createOscillator();
            fifth.type = "sine";
            fifth.start(0);
            const fifthGain = context.createGain();
            fifthGain.gain.setValueAtTime(0.7, 0);
            fifth.connect(fifthGain);

            const sub = context.createOscillator();
            sub.type = "triangle";
            sub.start(0);
            const subGain = context.createGain();
            subGain.gain.setValueAtTime(0.9, 0);
            sub.connect(subGain);

            const sub2 = context.createOscillator();
            sub2.type = "triangle";
            sub2.start(0);
            const sub2Gain = context.createGain();
            sub2Gain.gain.setValueAtTime(0.8, 0);
            sub2.connect(sub2Gain);

            const gain = context.createGain();
            gain.gain.setValueAtTime(0, 0);
            root.connect(gain);
            third.connect(gain);
            fifthGain.connect(gain);
            subGain.connect(gain);
            sub2Gain.connect(gain);

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

        this.compressor.connect(context.gain);
    }
}
