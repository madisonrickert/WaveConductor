import { SketchAudioContext } from "@/sketch";
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
    const { HeightMap, isTimeFast } = opts;
    const backgroundAudio = document.createElement("audio");
    backgroundAudio.autoplay = true;
    backgroundAudio.loop = true;

    const backgroundAudioSourceMp3 = document.createElement("source");
    backgroundAudioSourceMp3.src = wavesBackgroundAudioMP3;
    backgroundAudioSourceMp3.type = "audio/mp3";
    backgroundAudio.appendChild(backgroundAudioSourceMp3);

    const backgroundAudioSourceOgg = document.createElement("source");
    backgroundAudioSourceOgg.src = wavesBackgroundAudioOGG;
    backgroundAudioSourceOgg.type = "audio/ogg";
    backgroundAudio.appendChild(backgroundAudioSourceOgg);

    const sourceNode = audioContext.createMediaElementSource(backgroundAudio);
    document.body.appendChild(backgroundAudio);

    const backgroundAudioGain = audioContext.createGain();
    backgroundAudioGain.gain.setValueAtTime(0.0, 0);
    sourceNode.connect(backgroundAudioGain);
    backgroundAudioGain.connect(audioContext.gain);

    const noise = (() => {
        const node = audioContext.createBufferSource()
        , buffer = audioContext.createBuffer(1, audioContext.sampleRate * 5, audioContext.sampleRate)
        , data = buffer.getChannelData(0);
        for (let i = 0; i < buffer.length; i++) {
            data[i] = Math.random() * 2 - 1;
        }
        node.buffer = buffer;
        node.loop = true;
        node.start(0);
        return node;
    })();

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
    return {
        biquadFilter,
        dispose() {
            // Stop and disconnect noise buffer source
            try {
                noise.stop();
                noise.disconnect();
            } catch (_e) {
                // May already be stopped
            }

            // Disconnect script processor (deprecated but still needs cleanup)
            try {
                biquadFilter.disconnect();
                // Clear the processor callback to prevent further processing
                biquadFilter.onaudioprocess = null;
            } catch (_e) {
                // May already be disconnected
            }

            // Disconnect other audio nodes
            const audioNodes = [sourceNode, backgroundAudioGain, biquadFilterGain];
            audioNodes.forEach(node => {
                try {
                    node.disconnect();
                } catch (_e) {
                    // Node may already be disconnected
                }
            });

            // Clean up DOM element - this was appended to document.body
            backgroundAudio.pause();
            backgroundAudio.remove();
        },
    };
}
