import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { DevSettingsPanel } from './index';
import { SketchSettingsContext, SketchSettingsContextValue } from '@/common/hooks/useSketchSettings';

function renderWithContext(value: SketchSettingsContextValue) {
  return render(
    <SketchSettingsContext.Provider value={value}>
      <DevSettingsPanel />
    </SketchSettingsContext.Provider>
  );
}

describe('DevSettingsPanel', () => {
  it('renders global dev settings even when there are no per-sketch dev settings', () => {
    renderWithContext({
      defs: {
        name: { default: 'hello', category: 'user', label: 'Name' },
      },
      settings: { name: 'hello' },
      sketchId: 'test',
      setSetting: vi.fn(),
    });
    // Global dev settings (e.g. leapBackground) should still appear
    expect(screen.getByText('Leap: receive frames when tab is not focused')).toBeInTheDocument();
  });

  it('renders a row for each dev-category setting', () => {
    renderWithContext({
      defs: {
        speed: { default: 1, category: 'dev', label: 'Speed' },
        gravity: { default: 9.8, category: 'dev', label: 'Gravity' },
      },
      settings: { speed: 1, gravity: 9.8 },
      sketchId: 'test',
      setSetting: vi.fn(),
    });
    expect(screen.getByText('Speed')).toBeInTheDocument();
    expect(screen.getByText('Gravity')).toBeInTheDocument();
  });

  it('renders number input for number defaults', () => {
    renderWithContext({
      defs: {
        speed: { default: 1, category: 'dev', label: 'Speed', step: 0.1 },
      },
      settings: { speed: 5 },
      sketchId: 'test',
      setSetting: vi.fn(),
    });
    const input = screen.getByDisplayValue('5') as HTMLInputElement;
    expect(input.type).toBe('number');
    expect(input.step).toBe('0.1');
  });

  it('renders text input for string defaults', () => {
    renderWithContext({
      defs: {
        name: { default: 'hello', category: 'dev', label: 'Name' },
      },
      settings: { name: 'world' },
      sketchId: 'test',
      setSetting: vi.fn(),
    });
    const input = screen.getByDisplayValue('world') as HTMLInputElement;
    expect(input.type).toBe('text');
  });

  it('calls setSetting when number input value changes', async () => {
    const setSetting = vi.fn();
    renderWithContext({
      defs: {
        speed: { default: 1, category: 'dev', label: 'Speed' },
      },
      settings: { speed: 1 },
      sketchId: 'test',
      setSetting,
    });
    const input = screen.getByDisplayValue('1');
    await userEvent.clear(input);
    await userEvent.type(input, '42');
    expect(setSetting).toHaveBeenCalled();
  });
});
