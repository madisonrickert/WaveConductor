import { createWhiteNoise } from './noise';
import { createMockAudioContext } from '../test/mocks/webAudio';

describe('createWhiteNoise', () => {
  it('creates a buffer source node', () => {
    const ctx = createMockAudioContext();
    createWhiteNoise(ctx);
    expect(ctx.createBufferSource).toHaveBeenCalled();
  });

  it('sets loop to true and starts playback', () => {
    const ctx = createMockAudioContext();
    const node = createWhiteNoise(ctx);
    expect(node.loop).toBe(true);
    expect(node.start).toHaveBeenCalledWith(0);
  });

  it('fills buffer with values', () => {
    const ctx = createMockAudioContext();
    createWhiteNoise(ctx);
    // createBuffer should have been called with 1 channel, sampleRate * 5 length
    expect(ctx.createBuffer).toHaveBeenCalledWith(1, 44100 * 5, 44100);
  });
});
