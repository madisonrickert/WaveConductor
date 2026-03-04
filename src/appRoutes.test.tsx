import { render, screen } from '@testing-library/react';
import { MemoryRouter } from 'react-router';
import { AppRoutes } from './appRoutes';
import { AudioContextContext, AudioContextValue } from '@/common/hooks/useAudioContext';

// Mock SketchComponent to avoid WebGL dependencies
vi.mock('./components/sketchComponent', () => ({
  SketchComponent: ({ sketchClass }: { sketchClass: { id?: string; name: string } }) => (
    <div data-testid="sketch-component">{sketchClass.id ?? sketchClass.name}</div>
  ),
}));

// Mock sketches to avoid importing full sketch classes
vi.mock('./sketches', () => ({
  LineSketch: { id: 'line', name: 'LineSketch' },
  FlameSketch: { id: 'flame', name: 'FlameSketch' },
  Dots: { id: 'dots', name: 'Dots' },
  Cymatics: { id: 'cymatics', name: 'Cymatics' },
  Waves: { id: 'waves', name: 'Waves' },
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

  it.each(['/line', '/flame', '/dots', '/cymatics', '/waves'])('renders SketchComponent at %s', (path) => {
    renderAtRoute(path);
    expect(screen.getByTestId('sketch-component')).toBeInTheDocument();
  });

  it('renders LicensesPage at /licenses', () => {
    renderAtRoute('/licenses');
    // LicensesPage should render something - just check it doesn't crash
    expect(screen.queryByText('CharGallery')).not.toBeInTheDocument();
  });
});
