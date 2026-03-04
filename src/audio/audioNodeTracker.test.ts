import { AudioNodeTracker } from './audioNodeTracker';
import { createMockAudioContext } from '../test/mocks/webAudio';

describe('AudioNodeTracker', () => {
  let tracker: AudioNodeTracker;
  let ctx: AudioContext;

  beforeEach(() => {
    tracker = new AudioNodeTracker();
    ctx = createMockAudioContext();
  });

  describe('createOsc', () => {
    it('creates an oscillator with default frequency 440 and type sine', () => {
      const { osc } = tracker.createOsc(ctx);
      expect(ctx.createOscillator).toHaveBeenCalled();
      expect(osc.frequency.setValueAtTime).toHaveBeenCalledWith(440, 0);
      expect(osc.type).toBe('sine');
    });

    it('starts the oscillator immediately', () => {
      const { osc } = tracker.createOsc(ctx);
      expect(osc.start).toHaveBeenCalledWith(0);
    });

    it('connects the oscillator to the gain node', () => {
      const { osc } = tracker.createOsc(ctx);
      expect(osc.connect).toHaveBeenCalled();
    });

    it('respects custom frequency, type, and gain options', () => {
      const { osc, gain } = tracker.createOsc(ctx, { frequency: 880, type: 'square', gain: 0.5 });
      expect(osc.frequency.setValueAtTime).toHaveBeenCalledWith(880, 0);
      expect(osc.type).toBe('square');
      expect(gain.gain.setValueAtTime).toHaveBeenCalledWith(0.5, 0);
    });
  });

  describe('dispose', () => {
    it('stops and disconnects tracked sources', () => {
      const { osc } = tracker.createOsc(ctx);
      tracker.dispose();
      expect(osc.stop).toHaveBeenCalled();
      expect(osc.disconnect).toHaveBeenCalled();
    });

    it('disconnects tracked nodes', () => {
      const node = { disconnect: vi.fn() } as unknown as AudioNode;
      tracker.trackNode(node);
      tracker.dispose();
      expect(node.disconnect).toHaveBeenCalled();
    });

    it('is safe to call twice', () => {
      tracker.createOsc(ctx);
      tracker.dispose();
      expect(() => tracker.dispose()).not.toThrow();
    });
  });
});
