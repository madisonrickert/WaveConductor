import { SketchAudioContext } from "@/sketch";
import { AudioClip, AudioNodeTracker, createWhiteNoise } from "@/audio";
import { map } from "@/common/math";

import wavesBackgroundAudioMP3 from "./audio/waves_background.mp3";
import wavesBackgroundAudioOGG from "./audio/waves_background.ogg";

// return a number from [0..1] indicating in general how dark the image is; 1.0 means very dark, while 0.0 means very light
function getDarkness(frame: number) {
    if (frame % 1000 < 500) {
        return map(frame % 500, 0, 500, 0, 1);
    } else {
        return map(frame % 500, 0, 500, 1, 0);
    }
}

export interface WavesSketchAudioGroup {
    biquadFilter: ScriptProcessorNode;
    dispose(): void;
}

export function createAudioGroup(
    audioContext: SketchAudioContext,
    opts: {
        HeightMap: { frame: number; getWaviness: (frame: number) => number },
        isTimeFast: () => boolean
    }
): WavesSketchAudioGroup {
    const tracker = new AudioNodeTracker();
    const { HeightMap, isTimeFast } = opts;

    const backgroundAudio = new AudioClip({
        context: audioContext,
        srcs: [wavesBackgroundAudioMP3, wavesBackgroundAudioOGG],
        autoplay: true,
        loop: true,
        volume: 1.0,
    });

    const backgroundAudioGain = audioContext.createGain();
    backgroundAudioGain.gain.setValueAtTime(0.0, 0);
    backgroundAudio.getNode().connect(backgroundAudioGain);
    backgroundAudioGain.connect(audioContext.gain);

    const noise = createWhiteNoise(audioContext);
    tracker.trackSource(noise);

    const biquadFilter = (() => {
        const node = audioContext.createScriptProcessor(undefined, 1, 1);
        let a0 = 1;
        let b1 = 0;

        function setBiquadParameters(frame: number) {
            a0 = getDarkness(frame + 10) * 0.8;
            b1 = map(Math.pow(HeightMap.getWaviness(frame), 2), 0, 1, -0.92, -0.27);
            backgroundAudioGain.gain.setTargetAtTime(map(getDarkness(frame + 10), 0, 1, 1, 0.8), audioContext.currentTime, 0.016);
        }

        node.onaudioprocess = (e) => {
            const input = e.inputBuffer.getChannelData(0);
            const output = e.outputBuffer.getChannelData(0);
            const framesPerSecond = isTimeFast() ? 60 * 4 : 60;
            for (let n = 0; n < e.inputBuffer.length; n++) {
                if (n % 512 === 0) {
                    const frameOffset = n / audioContext.sampleRate * framesPerSecond;
                    setBiquadParameters(HeightMap.frame + frameOffset);
                }
                const x = input[n];
                const y1 = output[n - 1] || 0;

                output[n] = a0 * x - b1 * y1;
            }
        };
        return node;
    })();
    noise.connect(biquadFilter);

    const biquadFilterGain = audioContext.createGain();
    biquadFilterGain.gain.setValueAtTime(0.01, 0);
    biquadFilter.connect(biquadFilterGain);

    biquadFilterGain.connect(audioContext.gain);

    tracker.trackNode(backgroundAudioGain, biquadFilter, biquadFilterGain);

    return {
        biquadFilter,
        dispose() {
            biquadFilter.onaudioprocess = null;
            tracker.dispose();
            backgroundAudio.dispose();
        },
    };
}
