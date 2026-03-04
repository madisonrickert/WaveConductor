import { AudioClip } from './audioClip';
import { createMockAudioContext } from '../test/mocks/webAudio';

describe('AudioClip', () => {
  let ctx: AudioContext;

  beforeEach(() => {
    ctx = createMockAudioContext();
  });

  afterEach(() => {
    // Clean up any elements appended to body
    document.body.innerHTML = '';
  });

  describe('constructor', () => {
    it('creates an audio element and appends it to document.body', () => {
      new AudioClip({ context: ctx, srcs: ['test.mp3'] });
      const audioEl = document.querySelector('audio');
      expect(audioEl).toBeInTheDocument();
    });

    it('sets autoplay, loop, volume from options', () => {
      new AudioClip({ context: ctx, srcs: ['test.mp3'], autoplay: true, loop: true, volume: 0.5 });
      const audioEl = document.querySelector('audio')!;
      expect(audioEl.autoplay).toBe(true);
      expect(audioEl.loop).toBe(true);
      expect(audioEl.volume).toBe(0.5);
    });

    it('uses defaults when options not specified', () => {
      new AudioClip({ context: ctx, srcs: ['test.mp3'] });
      const audioEl = document.querySelector('audio')!;
      expect(audioEl.autoplay).toBe(false);
      expect(audioEl.loop).toBe(false);
      expect(audioEl.volume).toBe(1);
    });

    it('creates source elements with correct MIME types', () => {
      new AudioClip({ context: ctx, srcs: ['test.mp3', 'test.ogg', 'test.wav'] });
      const sources = document.querySelectorAll('source');
      expect(sources).toHaveLength(3);
      expect(sources[0].type).toBe('audio/mpeg');
      expect(sources[1].type).toBe('audio/ogg');
      expect(sources[2].type).toBe('audio/wav');
    });

    it('falls back to audio/extension for unknown extensions', () => {
      new AudioClip({ context: ctx, srcs: ['test.xyz'] });
      const source = document.querySelector('source')!;
      expect(source.type).toBe('audio/xyz');
    });
  });

  describe('volume', () => {
    it('get/set proxies to the audio element', () => {
      const clip = new AudioClip({ context: ctx, srcs: ['test.mp3'], volume: 0.8 });
      expect(clip.volume).toBe(0.8);
      clip.volume = 0.3;
      expect(clip.volume).toBe(0.3);
    });
  });

  describe('dispose', () => {
    it('removes element from DOM', () => {
      const clip = new AudioClip({ context: ctx, srcs: ['test.mp3'] });
      expect(document.querySelector('audio')).toBeInTheDocument();
      clip.dispose();
      expect(document.querySelector('audio')).not.toBeInTheDocument();
    });
  });
});
