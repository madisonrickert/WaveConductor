import { renderHook } from '@testing-library/react';
import { createElement } from 'react';
import { useSketchSettings, SketchSettingsContext, SketchSettingsContextValue } from './useSketchSettings';

describe('useSketchSettings', () => {
  it('throws when used outside SketchSettingsProvider', () => {
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
    expect(() => renderHook(() => useSketchSettings())).toThrow(
      'useSketchSettings must be used within a SketchSettingsProvider'
    );
    spy.mockRestore();
  });

  it('returns the context value when used inside a provider', () => {
    const mockValue: SketchSettingsContextValue = {
      settings: { speed: 1 },
      defs: { speed: { default: 1, category: 'dev', label: 'Speed' } },
      sketchId: 'test',
      setSetting: vi.fn(),
    };
    const wrapper = ({ children }: { children: React.ReactNode }) =>
      createElement(SketchSettingsContext.Provider, { value: mockValue }, children);

    const { result } = renderHook(() => useSketchSettings(), { wrapper });
    expect(result.current).toBe(mockValue);
  });
});
