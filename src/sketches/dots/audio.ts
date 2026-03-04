import { createWhiteNoise, AudioNodeTracker, detuned } from "@/audio";
import { SketchAudioContext } from "@/sketch";

export interface DotSketchAudioGroup {
    sourceGain: GainNode;
    lfo: OscillatorNode;
    lfoGain: GainNode;
    filter: BiquadFilterNode;
    filter2: BiquadFilterNode;
    filterGain: GainNode;
    setFrequency(freq: number): void;
    setVolume(volume: number): void;
    dispose(): void;
}

export function createAudioGroup(audioContext: SketchAudioContext): DotSketchAudioGroup {
    const tracker = new AudioNodeTracker();

    // white noise
    const noise = createWhiteNoise(audioContext);
    tracker.trackSource(noise);
    const noiseGain = audioContext.createGain();
    noiseGain.gain.setValueAtTime(0, 0);
    noise.connect(noiseGain);

    const BASE_FREQUENCY = 164.82;
    const { gain: source1 } = tracker.createOsc(audioContext, {
        frequency: detuned(BASE_FREQUENCY / 2, 2),
        type: "triangle",
        gain: 0.3,
    });
    const { gain: source2 } = tracker.createOsc(audioContext, {
        frequency: BASE_FREQUENCY,
        type: "triangle",
        gain: 0.30,
    });

    const sourceGain = audioContext.createGain();
    sourceGain.gain.setValueAtTime(0.0, 0);

    const { osc: lfo, gain: lfoGain } = tracker.createOsc(audioContext, {
        frequency: 8.66,
        gain: 0,
    });

    const filter = audioContext.createBiquadFilter();
    filter.type = "lowpass";
    filter.frequency.setValueAtTime(0, 0);
    filter.Q.setValueAtTime(5.18, 0);

    const filter2 = audioContext.createBiquadFilter();
    filter2.type = "bandpass";
    filter2.frequency.setValueAtTime(0, 0);
    filter2.Q.setValueAtTime(5.18, 0);

    const filterGain = audioContext.createGain();
    filterGain.gain.setValueAtTime(0.7, 0);

    source1.connect(sourceGain);
    source2.connect(sourceGain);
    sourceGain.connect(filter);

    lfoGain.connect(filter.frequency);
    lfoGain.connect(filter2.frequency);
    filter.connect(filter2);
    filter2.connect(filterGain);

    noiseGain.connect(audioContext.gain);
    filterGain.connect(audioContext.gain);

    tracker.trackNode(noiseGain, sourceGain, filter, filter2, filterGain);

    return {
        sourceGain,
        lfo,
        lfoGain,
        filter,
        filter2,
        filterGain,
        setFrequency(freq: number) {
            filter.frequency.setTargetAtTime(freq, audioContext.currentTime, 0.016);
            filter2.frequency.setTargetAtTime(freq, audioContext.currentTime, 0.016);
            lfoGain.gain.setTargetAtTime(freq * .06, audioContext.currentTime, 0.016);
        },
        setVolume(volume: number) {
            sourceGain.gain.setValueAtTime(volume, 0);
            noiseGain.gain.setValueAtTime(volume * 0.05, 0);
        },
        dispose() {
            tracker.dispose();
        },
    };
}
