import { AudioClip, createWhiteNoise, AudioNodeTracker, detuned, semitone } from "@/audio";
import { SketchAudioContext } from "@/sketch";

import audioBackgroundMp3 from "./audio/line_background.mp3";
import audioBackgroundOgg from "./audio/line_background.ogg";

export interface LineSketchAudioGroup {
    analyser: AnalyserNode;
    chordGain: GainNode;
    sourceGain: GainNode;
    sourceLfo: OscillatorNode;
    lfoGain: GainNode;
    filter: BiquadFilterNode;
    filter2: BiquadFilterNode;
    filterGain: GainNode;
    setFrequency: (freq: number) => void;
    setNoiseFrequency: (freq: number) => void;
    setVolume: (volume: number) => void;
    setBackgroundVolume: (volume: number) => void;
    dispose: () => void;
}

export function createAudioGroup(ctx: SketchAudioContext): LineSketchAudioGroup {
    const tracker = new AudioNodeTracker();

    const backgroundAudio = new AudioClip({
        context: ctx,
        srcs: [
            audioBackgroundMp3,
            audioBackgroundOgg,
        ],
        autoplay: true,
        loop: true,
        volume: 1.0,
    });

    backgroundAudio.getNode().connect(ctx.gain);

    // white noise
    const noise = createWhiteNoise(ctx);
    tracker.trackSource(noise);

    const noiseSourceGain = ctx.createGain();
    noiseSourceGain.gain.setValueAtTime(0, 0);
    noise.connect(noiseSourceGain);

    const noiseFilter = ctx.createBiquadFilter();
    noiseFilter.type = "lowpass";
    noiseFilter.frequency.setValueAtTime(0, 0);
    noiseFilter.Q.setValueAtTime(1.0, 0);
    noiseSourceGain.connect(noiseFilter);

    const noiseShelf = ctx.createBiquadFilter();
    noiseShelf.type = "lowshelf";
    noiseShelf.frequency.setValueAtTime(2200, 0);
    noiseShelf.gain.setValueAtTime(8, 0);
    noiseFilter.connect(noiseShelf);

    const noiseGain = ctx.createGain();
    noiseGain.gain.setValueAtTime(1.0, 0);
    noiseShelf.connect(noiseGain);

    const BASE_FREQUENCY = 320;

    const { gain: source1 } = tracker.createOsc(ctx, {
        frequency: detuned(BASE_FREQUENCY / 2, 2),
        type: "square",
        gain: 0.30,
    });
    const { gain: source2 } = tracker.createOsc(ctx, {
        frequency: BASE_FREQUENCY,
        type: "sawtooth",
        gain: 0.30,
    });
    const { gain: sourceLow } = tracker.createOsc(ctx, {
        frequency: BASE_FREQUENCY / 4,
        type: "sawtooth",
        gain: 0.90,
    });

    function makeChordSource(baseFrequency: number) {
        const intervals = [
            { freq: baseFrequency, type: "sine" as OscillatorType },
            { freq: semitone(baseFrequency, 12), type: "sawtooth" as OscillatorType },
            { freq: semitone(baseFrequency, 12 + 7), type: "sawtooth" as OscillatorType },
            { freq: semitone(baseFrequency, 24), type: "sawtooth" as OscillatorType },
            { freq: semitone(baseFrequency, 24 + 4), type: "sine" as OscillatorType },
        ];

        const gain = ctx.createGain();
        gain.gain.setValueAtTime(0.0, 0);

        for (const { freq, type } of intervals) {
            const { gain: oscGain } = tracker.createOsc(ctx, { frequency: freq, type });
            oscGain.connect(gain);
        }

        tracker.trackNode(gain);
        return gain;
    }
    const chordSource = makeChordSource(BASE_FREQUENCY);
    const chordHigh = makeChordSource(BASE_FREQUENCY * 8);

    const sourceGain = ctx.createGain();
    sourceGain.gain.setValueAtTime(0.0, 0);

    const { osc: sourceLfo, gain: lfoGain } = tracker.createOsc(ctx, {
        frequency: 8.66,
        gain: 0,
    });

    const filter = ctx.createBiquadFilter();
    filter.type = "bandpass";
    filter.frequency.setValueAtTime(0, 0);
    filter.Q.setValueAtTime(2.18, 0);

    const filter2 = ctx.createBiquadFilter();
    filter2.type = "bandpass";
    filter2.frequency.setValueAtTime(0, 0);
    filter2.Q.setValueAtTime(2.18, 0);

    const filterGain = ctx.createGain();
    filterGain.gain.setValueAtTime(0.4, 0);

    chordSource.connect(sourceGain);
    source1.connect(sourceGain);
    source2.connect(sourceGain);
    sourceLow.connect(sourceGain);
    chordHigh.connect(filter);
    sourceGain.connect(filter);

    lfoGain.connect(filter.frequency);
    lfoGain.connect(filter2.frequency);
    filter.connect(filter2);
    filter2.connect(filterGain);

    const audioGain = ctx.createGain();
    audioGain.gain.setValueAtTime(1.0, 0);

    noiseGain.connect(audioGain);
    filterGain.connect(audioGain);

    const analyser = ctx.createAnalyser();
    audioGain.connect(analyser);

    const compressor = ctx.createDynamicsCompressor();
    compressor.threshold.setValueAtTime(-50, 0);
    compressor.knee.setValueAtTime(12, 0);
    compressor.ratio.setValueAtTime(2, 0);
    analyser.connect(compressor);

    const highAttenuation = ctx.createBiquadFilter();
    highAttenuation.type = "highshelf";
    highAttenuation.frequency.setValueAtTime(BASE_FREQUENCY * 4, 0);
    highAttenuation.gain.setValueAtTime(-6, 0);
    compressor.connect(highAttenuation);

    const highAttenuation2 = ctx.createBiquadFilter();
    highAttenuation2.type = "highshelf";
    highAttenuation2.frequency.setValueAtTime(BASE_FREQUENCY * 8, 0);
    highAttenuation2.gain.setValueAtTime(-6, 0);
    highAttenuation.connect(highAttenuation2);

    highAttenuation2.connect(ctx.gain);

    tracker.trackNode(
        noiseSourceGain, noiseFilter, noiseShelf, noiseGain,
        sourceGain, filter, filter2, filterGain,
        audioGain, analyser, compressor, highAttenuation, highAttenuation2
    );

    return {
        analyser,
        chordGain: chordSource,
        sourceGain,
        sourceLfo,
        lfoGain,
        filter,
        filter2,
        filterGain,
        setFrequency(freq: number) {
            filter.frequency.cancelScheduledValues(ctx.currentTime);
            filter.frequency.setTargetAtTime(freq, ctx.currentTime, 0.016);

            filter2.frequency.cancelScheduledValues(ctx.currentTime);
            filter2.frequency.setTargetAtTime(freq, ctx.currentTime, 0.016);

            lfoGain.gain.cancelScheduledValues(ctx.currentTime);
            lfoGain.gain.setTargetAtTime(freq * .06, ctx.currentTime, 0.016);
        },
        setNoiseFrequency(freq: number) {
            noiseFilter.frequency.cancelScheduledValues(ctx.currentTime);
            noiseFilter.frequency.setTargetAtTime(freq, ctx.currentTime, 0.016);
        },
        setVolume(volume: number) {
            sourceGain.gain.cancelScheduledValues(ctx.currentTime);
            sourceGain.gain.setTargetAtTime(volume / 6, ctx.currentTime, 0.016);
            noiseSourceGain.gain.cancelScheduledValues(ctx.currentTime);
            noiseSourceGain.gain.setTargetAtTime(volume * 0.05, ctx.currentTime, 0.016);
            chordSource.gain.cancelScheduledValues(ctx.currentTime);
            chordSource.gain.setTargetAtTime(0.10, ctx.currentTime, 0.016);
            chordHigh.gain.cancelScheduledValues(ctx.currentTime);
            chordHigh.gain.setTargetAtTime(volume / 30, ctx.currentTime, 0.016);
        },
        setBackgroundVolume(volume: number) {
            backgroundAudio.volume = volume;
        },
        dispose() {
            tracker.dispose();
            backgroundAudio.dispose();
        }
    };
}
