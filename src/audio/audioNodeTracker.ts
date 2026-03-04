/**
 * Tracks audio nodes (oscillators, buffer sources, and generic nodes) for bulk disposal.
 * Eliminates the repeated try/catch boilerplate across sketch audio modules.
 */
export class AudioNodeTracker {
    private stoppables: AudioScheduledSourceNode[] = [];
    private nodes: AudioNode[] = [];

    /**
     * Creates an oscillator, starts it, connects it to a gain node, and tracks both.
     * Returns the gain node for further routing.
     */
    createOsc(
        ctx: BaseAudioContext,
        opts: { frequency?: number; type?: OscillatorType; gain?: number } = {}
    ): { osc: OscillatorNode; gain: GainNode } {
        const osc = ctx.createOscillator();
        osc.frequency.setValueAtTime(opts.frequency ?? 440, 0);
        osc.type = opts.type ?? "sine";
        osc.start(0);

        const gain = ctx.createGain();
        gain.gain.setValueAtTime(opts.gain ?? 1, 0);
        osc.connect(gain);

        this.stoppables.push(osc);
        this.nodes.push(gain);
        return { osc, gain };
    }

    /** Track a source node (oscillator or buffer source) that needs stop() + disconnect(). */
    trackSource(node: AudioScheduledSourceNode): void {
        this.stoppables.push(node);
    }

    /** Track a generic audio node that only needs disconnect(). */
    trackNode(...nodesToTrack: AudioNode[]): void {
        this.nodes.push(...nodesToTrack);
    }

    /** Stop all sources and disconnect all tracked nodes. Safe to call multiple times. */
    dispose(): void {
        for (const node of this.stoppables) {
            try { node.stop(); } catch { /* already stopped */ }
            try { node.disconnect(); } catch { /* already disconnected */ }
        }
        for (const node of this.nodes) {
            try { node.disconnect(); } catch { /* already disconnected */ }
        }
        this.stoppables = [];
        this.nodes = [];
    }
}
