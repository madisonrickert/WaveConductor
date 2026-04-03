import { renderHook } from '@testing-library/react';
import { createElement } from 'react';
import { useAudioContext, AudioContextContext, AudioContextValue } from './useAudioContext';

describe('useAudioContext', () => {
  it('throws when used outside AudioContextProvider', () => {
    // Suppress console.error for expected error
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
    expect(() => renderHook(() => useAudioContext())).toThrow(
      'useAudioContext must be used within AudioContextProvider'
    );
    spy.mockRestore();
  });

  it('returns the context value when used inside a provider', () => {
    const mockValue: AudioContextValue = {
      audioContext: {} as never,
      setUserVolume: vi.fn(),
    };
    const wrapper = ({ children }: { children: React.ReactNode }) =>
      createElement(AudioContextContext.Provider, { value: mockValue }, children);

    const { result } = renderHook(() => useAudioContext(), { wrapper });
    expect(result.current).toBe(mockValue);
  });
});
