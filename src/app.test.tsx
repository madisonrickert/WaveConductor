import { render, screen, act } from '@testing-library/react';
import App from './app';

// Mock AudioContext since AudioContextProvider creates a real one
vi.mock('@/audio/AudioContextProvider', () => ({
  AudioContextProvider: ({ children }: { children: React.ReactNode }) => <>{children}</>,
}));

// Mock SketchView to avoid WebGL dependencies
vi.mock('@/sketch/SketchView', () => ({
  SketchView: () => <div data-testid="sketch-component" />,
}));

describe('App', () => {
  it('renders without crashing', async () => {
    await act(async () => {
      render(<App />);
    });
    // The homepage should render by default (HashRouter starts at /)
    expect(screen.getByText('CharGallery')).toBeInTheDocument();
  });
});
