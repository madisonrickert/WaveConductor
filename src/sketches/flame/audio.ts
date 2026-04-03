import * as THREE from "three";
import { createWhiteNoise, AudioNodeTracker } from "@/audio";
import { map } from "@/math";
import { SketchAudioContext } from "@/sketch/BaseSketch";
import { Chord } from "./types";

const ROOT_FREQ = 120;
const MAJOR_SCALE = [0, 2, 4, 5, 7, 9, 11];
const MINOR_SCALE = [0, 2, 3, 5, 7, 8, 10];

/**
 * Creates a chord instrument: 5 oscillators (root, third, fifth, sub, sub2)
 * summed through a gain node. Supports major/minor switching and scale degree
 * transposition.
 */
function createChord(context: AudioContext, tracker: AudioNodeTracker): Chord {
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

    let minorBias = 0;
    let fifthBias = 0;
    let baseScaleDegree = 0;
    let isMajor = true;

    function getSemitoneNumber(scaleIndex: number) {
        const scale = isMajor ? MAJOR_SCALE : MINOR_SCALE;
        const octave = Math.floor(scaleIndex / scale.length);
        const pitchClass = scaleIndex % scale.length;
        return octave * 12 + scale[pitchClass];
    }

    function getFreq(semitoneNumber: number) {
        return ROOT_FREQ * Math.pow(2, semitoneNumber / 12);
    }

    function recompute() {
        const rootSemitone = getSemitoneNumber(baseScaleDegree);
        root.frequency.setValueAtTime(getFreq(rootSemitone), 0);

        const thirdSemitone = getSemitoneNumber(baseScaleDegree + 3) - minorBias;
        third.frequency.setValueAtTime(getFreq(thirdSemitone), 0);

        const fifthSemitone = getSemitoneNumber(baseScaleDegree + 5) + fifthBias;
        fifth.frequency.setValueAtTime(getFreq(fifthSemitone), 0);

        sub.frequency.setValueAtTime(getFreq(rootSemitone) / 2, 0);
        sub2.frequency.setValueAtTime(getFreq(rootSemitone) / 4, 0);
    }

    return {
        root, third, fifth, gain,
        setIsMajor: (major: boolean) => { isMajor = major; recompute(); },
        setScaleDegree: (sd: number) => { baseScaleDegree = Math.round(sd); recompute(); },
        setMinorBias: (mB: number) => { minorBias = Math.round(mB); recompute(); },
        setFifthBias: (fB: number) => { fifthBias = Math.round(fB); recompute(); },
    };
}

/**
 * Audio engine for the Flame sketch. Manages a filtered noise layer, oscillator
 * layer, and a 5-voice chord instrument, all routed through a dynamics compressor.
 *
 * The audio character is derived from the user's name (via hash-based parameter
 * selection in {@link configureForName}) and modulated per-frame by fractal
 * statistics (velocity, density) in {@link updateFromFractalStats}.
 */
export class FlameAudio {
    private tracker: AudioNodeTracker;
    private noiseGain: GainNode;
    private oscGain: GainNode;
    private filter: BiquadFilterNode;
    private compressor: DynamicsCompressorNode;
    private chord: Chord;

    private noiseGainScale = 0;
    private audioHasNoise = false;

    constructor(private context: SketchAudioContext) {
        const tracker = new AudioNodeTracker();
        this.tracker = tracker;

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

        const { gain: oscLowGain } = tracker.createOsc(context, {
            frequency: 0,
            type: "square",
            gain: 0.6,
        });

        this.filter = context.createBiquadFilter();
        this.filter.type = "lowpass";
        this.filter.frequency.setValueAtTime(100, 0);
        this.filter.Q.setValueAtTime(2.18, 0);
        oscLowGain.connect(this.filter);

        const { gain: oscHighGain } = tracker.createOsc(context, {
            frequency: 0,
            type: "triangle",
            gain: 0.05,
        });

        this.oscGain = context.createGain();
        this.oscGain.gain.setValueAtTime(0.0, 0);
        this.filter.connect(this.oscGain);
        oscHighGain.connect(this.oscGain);
        this.oscGain.connect(this.compressor);

        this.chord = createChord(context, tracker);
        this.chord.gain.connect(this.compressor);

        tracker.trackNode(this.noiseGain, this.filter, this.oscGain, this.compressor);
        this.compressor.connect(context.gain);
    }

    /**
     * Configures audio parameters based on a name's hash values.
     * Deterministically maps the name to filter frequency/Q, chord major/minor,
     * noise presence, and noise gain scale.
     */
    configureForName(hash: number, hash2: number, hash3: number) {
        this.filter.frequency.setValueAtTime(map((hash2 % 2e12) / 2e12, 0, 1, 120, 400), 0);
        this.filter.Q.setValueAtTime(map((hash3 % 2e12) / 2e12, 0, 1, 5, 8), 0);
        this.noiseGainScale = map((hash2 * hash3 % 100) / 100, 0, 1, 0.5, 1);
        this.chord.setIsMajor(hash2 % 2 === 0);
        // Basically boolean randoms; we don't want mod 2 because the hashes
        // are related to each other at that small level
        this.audioHasNoise = (hash3 % 100) >= 50;
    }

    /**
     * Updates audio parameters based on the camera's distance from origin.
     * Closer camera = more compression and louder output.
     */
    updateForCamera(cameraDistance: number) {
        const t = this.context.currentTime;
        this.compressor.ratio.setTargetAtTime(1 + 0.5 / (1 + cameraDistance), t, 0.016);
        this.context.gain.gain.setTargetAtTime((1.0 / (1 + cameraDistance)) + 0.5, t, 0.016);
    }

    /**
     * Modulates audio based on per-frame fractal statistics: velocity drives
     * noise and oscillator amplitude, density drives chord pitch and timbre.
     */
    updateFromFractalStats(velocity: number, count: number, countDensity: number) {
        const density = countDensity / count;
        const velocityFactor = Math.min(velocity * this.noiseGainScale, 0.06);
        const t = this.context.currentTime;

        if (this.audioHasNoise) {
            const noiseAmplitude = 2 / (1 + density * density);
            const target = this.noiseGain.gain.value * 0.5 + 0.5 * (velocityFactor * noiseAmplitude + 1e-5);
            this.noiseGain.gain.setTargetAtTime(target, t, 0.016);
        }

        const newOscGain = this.oscGain.gain.value * 0.9 + 0.1 * Math.max(0, Math.min(velocity * velocity * 2000, 0.6) - 0.01);
        this.oscGain.gain.setTargetAtTime(newOscGain, t, 0.016);

        const baseOffset = THREE.MathUtils.clamp(Math.floor(map(density, 1.0, 3, 0, 24)), 0, 48);
        this.chord.setScaleDegree(baseOffset);
        const chordTarget = this.chord.gain.gain.value * 0.9 + 0.1 * (velocityFactor * count * count / 8 + 0.0001);
        this.chord.gain.gain.setTargetAtTime(chordTarget, t, 0.016);
    }

    dispose() {
        this.tracker.dispose();
    }
}
