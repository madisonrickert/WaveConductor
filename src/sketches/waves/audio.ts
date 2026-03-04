import { SketchAudioContext } from "@/common/sketch";
import { AudioClip, AudioNodeTracker, createWhiteNoise } from "@/audio";
import { map } from "@/common/math";

import wavesBackgroundAudioMP3 from "./audio/waves_background.mp3";
import wavesBackgroundAudioOGG from "./audio/waves_background.ogg";
import wavesProcessorUrl from "./waves-processor.ts?worker&url";

// return a number from [0..1] indicating in general how dark the image is; 1.0 means very dark, while 0.0 means very light
export function getDarkness(frame: number) {
    if (frame % 1000 < 500) {
        return map(frame % 500, 0, 500, 0, 1);
    } else {
        return map(frame % 500, 0, 500, 1, 0);
    }
}

export interface WavesSketchAudioGroup {
    updateParameters(): void;
    dispose(): void;
}

export function createAudioGroup(
    audioContext: SketchAudioContext,
    opts: {
        HeightMap: { frame: number; getWaviness: (frame: number) => number },
    }
): WavesSketchAudioGroup {
    const tracker = new AudioNodeTracker();
    const { HeightMap } = opts;

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

    const biquadFilterGain = audioContext.createGain();
    biquadFilterGain.gain.setValueAtTime(0.01, 0);
    biquadFilterGain.connect(audioContext.gain);

    // Load AudioWorklet asynchronously; noise stays disconnected until ready
    let workletNode: AudioWorkletNode | null = null;
    let disposed = false;

    audioContext.audioWorklet.addModule(wavesProcessorUrl).then(() => {
        if (disposed) return;

        workletNode = new AudioWorkletNode(audioContext, 'waves-biquad-processor');
        noise.connect(workletNode);
        workletNode.connect(biquadFilterGain);
        tracker.trackNode(workletNode);
    }).catch((err) => {
        console.error('Failed to load waves audio worklet:', err);
    });

    tracker.trackNode(backgroundAudioGain, biquadFilterGain);

    return {
        updateParameters() {
            const frame = HeightMap.frame;
            const darkness = getDarkness(frame + 10);
            const a0 = darkness * 0.8;
            const b1 = map(Math.pow(HeightMap.getWaviness(frame), 2), 0, 1, -0.92, -0.27);

            if (workletNode) {
                workletNode.parameters.get('a0')!.setValueAtTime(a0, audioContext.currentTime);
                workletNode.parameters.get('b1')!.setValueAtTime(b1, audioContext.currentTime);
            }

            backgroundAudioGain.gain.setTargetAtTime(
                map(darkness, 0, 1, 1, 0.8),
                audioContext.currentTime,
                0.016
            );
        },
        dispose() {
            disposed = true;
            tracker.dispose();
            backgroundAudio.dispose();
        },
    };
}
