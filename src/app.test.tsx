import { render, screen, act } from '@testing-library/react';
import App from './app';

// Mock AudioContext since AudioContextProvider creates a real one
vi.mock('./common/audioContext', () => ({
  AudioContextProvider: ({ children }: { children: React.ReactNode }) => <>{children}</>,
}));

// Mock SketchComponent to avoid WebGL dependencies
vi.mock('./components/sketchComponent', () => ({
  SketchComponent: () => <div data-testid="sketch-component" />,
}));

describe('App', () => {
  it('renders without crashing', async () => {
    await act(async () => {
      render(<App />);
    });
    // The homepage should render by default (HashRouter starts at /)
    expect(screen.getByText('hellochar')).toBeInTheDocument();
  });
});
