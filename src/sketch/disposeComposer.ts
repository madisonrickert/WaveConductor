import { EffectComposer } from "three-stdlib";

/**
 * Disposes an {@link EffectComposer} and all of its passes, releasing GPU
 * resources (shader programs, render targets). Each pass is disposed and
 * removed individually before the composer itself is disposed.
 */
export function disposeComposer(composer: EffectComposer): void {
    while (composer.passes.length > 0) {
        composer.passes[0].dispose();
        composer.removePass(composer.passes[0]);
    }
    composer.dispose();
}
