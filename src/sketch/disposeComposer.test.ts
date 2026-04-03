import { disposeComposer } from './disposeComposer';
import { EffectComposer } from 'three-stdlib';

function createMockComposer(passCount: number) {
    const passes = Array.from({ length: passCount }, () => ({ dispose: vi.fn() }));
    return {
        passes: [...passes],
        removePass(pass: unknown) {
            const idx = this.passes.indexOf(pass);
            if (idx >= 0) this.passes.splice(idx, 1);
        },
        dispose: vi.fn(),
        _originalPasses: passes,
    } as unknown as EffectComposer & { _originalPasses: { dispose: ReturnType<typeof vi.fn> }[] };
}

describe('disposeComposer', () => {
    it('disposes all passes and the composer', () => {
        const composer = createMockComposer(3);
        disposeComposer(composer);

        for (const pass of composer._originalPasses) {
            expect(pass.dispose).toHaveBeenCalledTimes(1);
        }
        expect(composer.dispose).toHaveBeenCalledTimes(1);
        expect(composer.passes).toHaveLength(0);
    });

    it('handles a composer with no passes', () => {
        const composer = createMockComposer(0);
        disposeComposer(composer);
        expect(composer.dispose).toHaveBeenCalledTimes(1);
    });
});
