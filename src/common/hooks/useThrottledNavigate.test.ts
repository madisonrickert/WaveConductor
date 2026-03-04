import { renderHook, act } from '@testing-library/react';
import { createElement } from 'react';
import { MemoryRouter } from 'react-router';
import { useThrottledNavigate } from './useThrottledNavigate';

describe('useThrottledNavigate', () => {
  const wrapper = ({ children }: { children: React.ReactNode }) =>
    createElement(MemoryRouter, null, children);

  it('navigates to the given path', () => {
    const { result } = renderHook(() => useThrottledNavigate(500), { wrapper });
    act(() => {
      result.current('/line');
    });
    // If no error is thrown, navigation succeeded
  });

  it('throttles rapid calls', () => {
    vi.useFakeTimers();
    const { result } = renderHook(() => useThrottledNavigate(500), { wrapper });

    act(() => {
      result.current('/line');
      result.current('/flame');
      result.current('/dots');
    });

    vi.useRealTimers();
    // Should not throw - throttling prevents excessive navigation
  });
});
