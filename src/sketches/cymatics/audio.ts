import * as THREE from "three";
import { throttle } from "radash";

import { AudioClip, createWhiteNoise, AudioNodeTracker } from "@/audio";
import { SketchAudioContext } from "@/sketch";

function makeAudioSrcs(fileName: string) {
    return [
        new URL(`./audio/${fileName}.webm`, import.meta.url).toString(),
        new URL(`./audio/${fileName}.mp3`, import.meta.url).toString(),
        new URL(`./audio/${fileName}.wav`, import.meta.url).toString(),
    ];
}

interface OscWithGain {
    osc: OscillatorNode;
    gain: GainNode;
}

export class CymaticsAudio {
    private kick: AudioClip;
    private risingBass: AudioClip;
    private blub: AudioClip;

    private oscBase: OscWithGain;
    private oscUnison: OscWithGain;
    private oscFifth: OscWithGain;
    private oscSub: OscWithGain;
    private oscHigh4: OscWithGain;
    private oscHigh4Second: OscWithGain;
    private whiteNoiseGain: GainNode;
    private whiteNoiseFilter: BiquadFilterNode;
    private lfo: OscWithGain;
    private lfoGain: GainNode;
    private oscGain: GainNode;
    private tracker: AudioNodeTracker;
    private debouncedTriggerJitter: () => void;

    constructor(public audio: SketchAudioContext) {
        this.tracker = new AudioNodeTracker();

        this.kick = new AudioClip({
            context: audio,
            srcs: makeAudioSrcs("kick"),
            volume: 0.3,
        });
        this.kick.getNode().connect(audio.gain);

        this.risingBass = new AudioClip({
            context: audio,
            srcs: makeAudioSrcs("risingbass"),
        });
        this.risingBass.getNode().connect(audio.gain);

        this.blub = new AudioClip({
            context: audio,
            srcs: makeAudioSrcs("blub"),
            volume: 0,
            autoplay: true,
            loop: true,
        });
        this.blub.getNode().connect(audio.gain);

        this.oscBase = this.tracker.createOsc(audio, { frequency: OSC_FREQ_BASE, gain: 1 });
        this.oscUnison = this.tracker.createOsc(audio, { frequency: OSC_FREQ_BASE, gain: 0.5 });
        this.oscFifth = this.tracker.createOsc(audio, { frequency: OSC_FREQ_BASE, gain: 0.5 });
        this.oscSub = this.tracker.createOsc(audio, { frequency: OSC_FREQ_BASE, gain: 0.5 });
        this.oscHigh4 = this.tracker.createOsc(audio, { frequency: OSC_FREQ_BASE, gain: 0.02 });
        this.oscHigh4Second = this.tracker.createOsc(audio, { frequency: OSC_FREQ_BASE, gain: 0.01 });

        this.oscGain = audio.createGain();
        this.oscGain.gain.setValueAtTime(0.0, 0);

        this.oscBase.gain.connect(this.oscGain);
        this.oscUnison.gain.connect(this.oscGain);
        this.oscFifth.gain.connect(this.oscGain);
        this.oscSub.gain.connect(this.oscGain);
        this.oscHigh4.gain.connect(this.oscGain);
        this.oscHigh4Second.gain.connect(this.oscGain);

        this.lfoGain = audio.createGain();
        this.oscGain.connect(this.lfoGain);

        this.lfo = this.tracker.createOsc(audio, { frequency: 1, gain: 0.5 });
        this.lfo.osc.connect(this.lfoGain.gain);

        this.lfoGain.connect(audio.gain);

        // White noise
        const whiteNoise = createWhiteNoise(this.audio);
        this.tracker.trackSource(whiteNoise);
        this.whiteNoiseGain = audio.createGain();
        this.whiteNoiseGain.gain.setValueAtTime(0.1, 0);
        whiteNoise.connect(this.whiteNoiseGain);

        this.whiteNoiseFilter = audio.createBiquadFilter();
        this.whiteNoiseFilter.type = "bandpass";
        this.whiteNoiseFilter.Q.setValueAtTime(100.0, 0);
        this.whiteNoiseGain.connect(this.whiteNoiseFilter);

        this.whiteNoiseFilter.connect(audio.gain);

        this.tracker.trackNode(this.oscGain, this.lfoGain, this.whiteNoiseGain, this.whiteNoiseFilter);

        this.setOscFrequencyScalar(1);

        this.debouncedTriggerJitter = throttle(
            { interval: 500 },
            () => {
                this.kick.play();
                this.risingBass.play();
            }
        );
    }

    triggerJitter() {
        this.debouncedTriggerJitter();
    }

    setBlubVolume(v: number) {
        this.blub.volume = THREE.MathUtils.clamp(v * 0.05, 0, 0.3);
    }

    setBlubPlaybackRate(r: number) {
        this.blub.playbackRate = THREE.MathUtils.clamp(r, 0.5, 4);
    }

    setOscVolume(v: number) {
        this.oscGain.gain.setTargetAtTime(THREE.MathUtils.clamp(v * 0.75, 1e-10, 1), this.audio.currentTime + 0.016, 0.016 / 3);
    }

    setOscFrequencyScalar(freqScalar: number) {
        const freq = OSC_FREQ_BASE * freqScalar;

        this.oscUnison.osc.frequency.setTargetAtTime(freq, this.audio.currentTime + 0.016, 0.016 / 3);
        this.oscFifth.osc.frequency.setTargetAtTime(freq * Math.pow(2, 7 / 12), this.audio.currentTime + 0.016, 0.016 / 3);
        this.oscSub.osc.frequency.setTargetAtTime(freq / 2, this.audio.currentTime + 0.016, 0.016 / 3);
        this.oscHigh4.osc.frequency.setTargetAtTime(freq * Math.pow(2, 4) + 4, this.audio.currentTime + 0.016, 0.016 / 3);
        this.oscHigh4Second.osc.frequency.setTargetAtTime(freq * freqScalar * Math.pow(2, 4 + 1 / 12) + 9, this.audio.currentTime + 0.016, 0.016 / 3);
        this.lfo.osc.frequency.setTargetAtTime((freqScalar - 1) * 100 + 1e-10, this.audio.currentTime + 0.016, 0.016 / 3);

        this.whiteNoiseGain.gain.setTargetAtTime(THREE.MathUtils.clamp((freqScalar - 1.002) * 20, 0, 1), this.audio.currentTime + 0.016, 0.016 / 3);
        this.whiteNoiseFilter.frequency.setTargetAtTime(1500 * (1 + freqScalar * freqScalar), this.audio.currentTime + 0.016, 0.016 / 3);
    }

    dispose() {
        this.tracker.dispose();
        this.kick.dispose();
        this.risingBass.dispose();
        this.blub.dispose();
    }
}

const OSC_FREQ_BASE = 126;
