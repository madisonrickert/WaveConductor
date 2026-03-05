import { SketchAudioContext } from "@/common/sketch";
import { AudioClip, AudioNodeTracker, createWhiteNoise } from "@/audio";
import { map } from "@/common/math";

import wavesBackgroundAudioMP3 from "./audio/waves_background.mp3";
import wavesBackgroundAudioOGG from "./audio/waves_background.ogg";
import wavesProcessorUrl from "./waves-processor.ts?worker&url";

/**
 * Returns a value from [0..1] indicating how dark the visual is at the given frame.
 * 1.0 = very dark, 0.0 = very light. Ramps up over 500 frames, then back down, with period 1000.
 */
export function getDarkness(frame: number) {
    if (frame % 1000 < 500) {
        return map(frame % 500, 0, 500, 0, 1);
    } else {
        return map(frame % 500, 0, 500, 1, 0);
    }
}

/** Controls for the Waves sketch audio, synced to the visual state each frame. */
export interface WavesSketchAudioGroup {
    /** Sync audio filter and gain parameters to the current heightmap state. Called once per frame. */
    updateParameters(): void;
    dispose(): void;
}

/**
 * Creates the audio processing chain for the Waves sketch.
 *
 * Signal chain:
 * - Background music: looping audio clip → gain (modulated by darkness) → master
 * - Noise layer: white noise → one-pole biquad filter (AudioWorklet) → gain → master
 *
 * The biquad filter's `a0` (input gain) scales with darkness, and `b1` (feedback coefficient)
 * scales with waviness², producing a brighter/harsher tone when the visual is more wavy.
 */
export function createAudioGroup(
    audioContext: SketchAudioContext,
    opts: {
        heightMap: { frame: number; cachedWaviness: number },
    }
): WavesSketchAudioGroup {
    const tracker = new AudioNodeTracker();
    const { heightMap } = opts;

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
            const frame = heightMap.frame;
            const darkness = getDarkness(frame + 10);
            const a0 = darkness * 0.8;
            const w = heightMap.cachedWaviness;
            const b1 = map(w * w, 0, 1, -0.92, -0.27);

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
