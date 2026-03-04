export function createMockAudioContext(): AudioContext {
  const mockGainNode = () => ({
    gain: { value: 1, setValueAtTime: vi.fn(), setTargetAtTime: vi.fn() },
    connect: vi.fn(),
    disconnect: vi.fn(),
  });

  return {
    createGain: vi.fn(mockGainNode),
    createOscillator: vi.fn(() => ({
      frequency: { value: 440, setValueAtTime: vi.fn() },
      type: 'sine' as OscillatorType,
      connect: vi.fn(),
      disconnect: vi.fn(),
      start: vi.fn(),
      stop: vi.fn(),
    })),
    createBufferSource: vi.fn(() => ({
      buffer: null,
      loop: false,
      connect: vi.fn(),
      disconnect: vi.fn(),
      start: vi.fn(),
      stop: vi.fn(),
    })),
    createBuffer: vi.fn((_channels: number, length: number, _sampleRate: number) => ({
      length,
      getChannelData: vi.fn(() => new Float32Array(length)),
    })),
    createMediaElementSource: vi.fn(() => ({
      connect: vi.fn(),
      disconnect: vi.fn(),
    })),
    destination: {},
    sampleRate: 44100,
    currentTime: 0,
    state: 'running',
    resume: vi.fn(),
    suspend: vi.fn(),
    close: vi.fn(),
    audioWorklet: { addModule: vi.fn().mockResolvedValue(undefined) },
  } as unknown as AudioContext;
}
