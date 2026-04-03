import { renderHook } from '@testing-library/react';
import { useSketchLifecycle } from './useSketchLifecycle';
import { Sketch } from '@/sketch/Sketch';

describe('useSketchLifecycle', () => {
  const makeMockSketch = () => ({
    init: vi.fn(),
    destroy: vi.fn(),
    animate: vi.fn(),
    renderer: {} as never,
    audioContext: {} as never,
  }) as unknown as Sketch;

  it('calls sketch.init() on mount', () => {
    const sketch = makeMockSketch();
    renderHook(() => useSketchLifecycle(sketch));
    expect(sketch.init).toHaveBeenCalledTimes(1);
  });

  it('calls sketch.destroy() on unmount', () => {
    const sketch = makeMockSketch();
    const { unmount } = renderHook(() => useSketchLifecycle(sketch));
    expect(sketch.destroy).not.toHaveBeenCalled();
    unmount();
    expect(sketch.destroy).toHaveBeenCalledTimes(1);
  });
});
