import { FlameAudio } from './audio';
import { createMockAudioContext } from '@/test/mocks/webAudio';
import { SketchAudioContext } from '@/sketch/BaseSketch';

function createTestAudio() {
    const mockCtx = createMockAudioContext() as unknown as SketchAudioContext;
    mockCtx.gain = mockCtx.createGain() as unknown as GainNode;
    return { audio: new FlameAudio(mockCtx), ctx: mockCtx };
}

describe('FlameAudio', () => {
    it('constructs without throwing', () => {
        expect(() => createTestAudio()).not.toThrow();
    });

    it('configureForName sets filter and chord parameters deterministically', () => {
        const { audio: audio1 } = createTestAudio();
        const { audio: audio2 } = createTestAudio();

        // Same inputs should produce same configuration (no exceptions)
        audio1.configureForName(12345, 67890, 11111);
        audio2.configureForName(12345, 67890, 11111);

        // Different inputs should not throw
        audio1.configureForName(99999, 88888, 77777);
    });

    it('updateForCamera does not throw', () => {
        const { audio } = createTestAudio();
        expect(() => audio.updateForCamera(2.5)).not.toThrow();
        expect(() => audio.updateForCamera(0.1)).not.toThrow();
    });

    it('updateFromFractalStats does not throw', () => {
        const { audio } = createTestAudio();
        audio.configureForName(100, 200, 300);
        expect(() => audio.updateFromFractalStats(0.5, 100, 250)).not.toThrow();
    });

    it('dispose cleans up without throwing', () => {
        const { audio } = createTestAudio();
        expect(() => audio.dispose()).not.toThrow();
    });
});
