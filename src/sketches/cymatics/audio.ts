import * as THREE from "three";
import debounce from "debounce";

import { AudioClip, createWhiteNoise } from "@/audio";
import { SketchAudioContext } from "@/sketch";

interface OscillatorWithGain extends OscillatorNode {
    gain: GainNode;
}

function makeAudioSrcs(fileName: string) {
    return [
        new URL(`./audio/${fileName}.webm`, import.meta.url).toString(),
        new URL(`./audio/${fileName}.mp3`, import.meta.url).toString(),
        new URL(`./audio/${fileName}.wav`, import.meta.url).toString(),
    ];
}

export class CymaticsAudio {
    private kick: AudioClip;
    private risingBass: AudioClip;
    private blub: AudioClip;

    private oscBase: OscillatorWithGain;
    private oscUnison: OscillatorWithGain;
    private oscFifth: OscillatorWithGain;
    private oscSub: OscillatorWithGain;
    private oscHigh4: OscillatorWithGain;
    private oscHigh4Second: OscillatorWithGain;
    private whiteNoise: AudioBufferSourceNode;
    private whiteNoiseGain: GainNode;
    private whiteNoiseFilter: BiquadFilterNode;
    private lfo: OscillatorWithGain;
    private lfoGain: GainNode;
    private oscGain: GainNode;
    private debouncedTriggerJitter: () => void;

    constructor(public audio: SketchAudioContext) {
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

        this.oscBase = this.makeOsc(1);
        this.oscUnison = this.makeOsc(0.5);
        this.oscFifth = this.makeOsc(0.5);
        this.oscSub = this.makeOsc(0.5);
        this.oscHigh4 = this.makeOsc(0.02);
        this.oscHigh4Second = this.makeOsc(0.01);

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

        this.lfo = this.makeOsc(0.5);
        this.lfo.frequency.setValueAtTime(1, 0);
        this.lfo.connect(this.lfoGain.gain);

        this.lfoGain.connect(audio.gain);

        this.whiteNoise = createWhiteNoise(this.audio);
        this.whiteNoiseGain = audio.createGain();
        this.whiteNoiseGain.gain.setValueAtTime(0.1, 0);
        this.whiteNoise.connect(this.whiteNoiseGain);

        this.whiteNoiseFilter = audio.createBiquadFilter();
        this.whiteNoiseFilter.type = "bandpass";
        this.whiteNoiseFilter.Q.setValueAtTime(100.0, 0);
        this.whiteNoiseGain.connect(this.whiteNoiseFilter);

        this.whiteNoiseFilter.connect(audio.gain);

        this.setOscFrequencyScalar(1);

        this.debouncedTriggerJitter = debounce(() => {
            this.kick.play();
            this.risingBass.play();
        }, 500);
    }

    private makeOsc(volume: number) {
        const osc = this.audio.createOscillator() as OscillatorWithGain;
        osc.type = "sine";
        osc.frequency.setValueAtTime(OSC_FREQ_BASE, 0);
        osc.start();

        const gain = this.audio.createGain();
        gain.gain.setValueAtTime(volume, 0);
        osc.connect(gain);

        osc.gain = gain;

        return osc;
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

        this.oscUnison.frequency.setTargetAtTime(freq, this.audio.currentTime + 0.016, 0.016 / 3);
        this.oscFifth.frequency.setTargetAtTime(freq * Math.pow(2, 7 / 12), this.audio.currentTime + 0.016, 0.016 / 3);
        this.oscSub.frequency.setTargetAtTime(freq / 2, this.audio.currentTime + 0.016, 0.016 / 3);
        this.oscHigh4.frequency.setTargetAtTime(freq * Math.pow(2, 4) + 4, this.audio.currentTime + 0.016, 0.016 / 3);
        this.oscHigh4Second.frequency.setTargetAtTime(freq * freqScalar * Math.pow(2, 4 + 1 / 12) + 9, this.audio.currentTime + 0.016, 0.016 / 3);
        this.lfo.frequency.setTargetAtTime((freqScalar - 1) * 100 + 1e-10, this.audio.currentTime + 0.016, 0.016 / 3);

        this.whiteNoiseGain.gain.setTargetAtTime(THREE.MathUtils.clamp((freqScalar - 1.002) * 20, 0, 1), this.audio.currentTime + 0.016, 0.016 / 3);
        this.whiteNoiseFilter.frequency.setTargetAtTime(1500 * (1 + freqScalar * freqScalar), this.audio.currentTime + 0.016, 0.016 / 3);
    }

    dispose() {
        // Stop and disconnect all oscillators
        const oscillators = [
            this.oscBase,
            this.oscUnison, 
            this.oscFifth,
            this.oscSub,
            this.oscHigh4,
            this.oscHigh4Second,
            this.lfo
        ];

        oscillators.forEach(osc => {
            try {
                osc.stop();
                osc.disconnect();
                osc.gain.disconnect();
            } catch (e) {
                // Oscillator may already be stopped/disconnected
            }
        });

        // Stop and disconnect white noise buffer source
        try {
            this.whiteNoise.stop();
            this.whiteNoise.disconnect();
        } catch (e) {
            // May already be stopped
        }

        // Disconnect all audio nodes
        const audioNodes = [
            this.whiteNoiseGain,
            this.whiteNoiseFilter,
            this.oscGain,
            this.lfoGain
        ];

        audioNodes.forEach(node => {
            try {
                node.disconnect();
            } catch (e) {
                // Node may already be disconnected
            }
        });

        // Dispose of audio clips
        this.kick.dispose();
        this.risingBass.dispose();
        this.blub.dispose();
    }
}

const OSC_FREQ_BASE = 126;
