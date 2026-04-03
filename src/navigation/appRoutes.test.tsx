import { render, screen } from '@testing-library/react';
import { MemoryRouter } from 'react-router';
import { AppRoutes } from '@/navigation/appRoutes';
import { AudioContextContext, AudioContextValue } from '@/audio/useAudioContext';

// Mock SketchView to avoid WebGL dependencies
vi.mock('@/sketch/SketchView', () => ({
  SketchView: ({ sketchClass }: { sketchClass: { id?: string; name: string } }) => (
    <div data-testid="sketch-component">{sketchClass.id ?? sketchClass.name}</div>
  ),
}));

// Mock sketches to avoid importing full sketch classes
vi.mock('./sketches', () => ({
  LineSketch: { id: 'line', name: 'LineSketch' },
  FlameSketch: { id: 'flame', name: 'FlameSketch' },
  DotsSketch: { id: 'dots', name: 'DotsSketch' },
  CymaticsSketch: { id: 'cymatics', name: 'CymaticsSketch' },
  WavesSketch: { id: 'waves', name: 'WavesSketch' },
}));

const mockAudioCtx = {
  audioContext: {} as never,
  setUserVolume: vi.fn(),
} as AudioContextValue;

function renderAtRoute(route: string) {
  return render(
    <MemoryRouter initialEntries={[route]}>
      <AudioContextContext.Provider value={mockAudioCtx}>
        <AppRoutes />
      </AudioContextContext.Provider>
    </MemoryRouter>
  );
}

describe('AppRoutes', () => {
  it('renders HomePage at /', () => {
    renderAtRoute('/');
    expect(screen.getByText('CharGallery')).toBeInTheDocument();
  });

  it.each(['/gravity', '/you-niverse', '/fabric', '/cymatics', '/waves'])('renders SketchView at %s', (path) => {
    renderAtRoute(path);
    expect(screen.getByTestId('sketch-component')).toBeInTheDocument();
  });

  it('renders LicensesPage at /licenses', () => {
    renderAtRoute('/licenses');
    // LicensesPage should render something - just check it doesn't crash
    expect(screen.queryByText('CharGallery')).not.toBeInTheDocument();
  });
});
