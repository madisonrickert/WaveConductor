import { loadSettings, saveSettings, saveSetting } from './sketchSettingsStore';
import { SettingsDefs } from './sketchSettings';

const testDefs = {
  speed: { default: 1, category: 'dev' as const, label: 'Speed' },
  color: { default: 'red', category: 'user' as const, label: 'Color' },
} satisfies SettingsDefs;

beforeEach(() => {
  localStorage.clear();
});

describe('loadSettings', () => {
  it('returns defaults when no saved data exists', () => {
    const result = loadSettings('test-sketch', testDefs);
    expect(result).toEqual({ speed: 1, color: 'red' });
  });

  it('returns saved values merged with defaults', () => {
    localStorage.setItem('sketch-settings:test-sketch', JSON.stringify({ speed: 5 }));
    const result = loadSettings('test-sketch', testDefs);
    expect(result).toEqual({ speed: 5, color: 'red' });
  });

  it('ignores unknown keys from saved data', () => {
    localStorage.setItem('sketch-settings:test-sketch', JSON.stringify({ speed: 5, unknown: 'x' }));
    const result = loadSettings('test-sketch', testDefs);
    expect(result).toEqual({ speed: 5, color: 'red' });
    expect(result).not.toHaveProperty('unknown');
  });

  it('falls back to defaults on invalid JSON', () => {
    localStorage.setItem('sketch-settings:test-sketch', 'not-json');
    const result = loadSettings('test-sketch', testDefs);
    expect(result).toEqual({ speed: 1, color: 'red' });
  });

  it('handles empty definitions', () => {
    const result = loadSettings('test-sketch', {});
    expect(result).toEqual({});
  });
});

describe('saveSettings', () => {
  it('writes JSON to localStorage with correct key prefix', () => {
    saveSettings('test-sketch', { speed: 10, color: 'blue' });
    const raw = localStorage.getItem('sketch-settings:test-sketch');
    expect(JSON.parse(raw!)).toEqual({ speed: 10, color: 'blue' });
  });

  it('overwrites previous values', () => {
    saveSettings('test-sketch', { speed: 1 });
    saveSettings('test-sketch', { speed: 99 });
    const raw = localStorage.getItem('sketch-settings:test-sketch');
    expect(JSON.parse(raw!)).toEqual({ speed: 99 });
  });
});

describe('saveSetting', () => {
  it('saves a single setting while preserving others', () => {
    saveSettings('test-sketch', { speed: 1, color: 'red' });
    saveSetting('test-sketch', testDefs, 'speed', 42);
    const result = loadSettings('test-sketch', testDefs);
    expect(result).toEqual({ speed: 42, color: 'red' });
  });

  it('creates entry if none exists yet', () => {
    saveSetting('test-sketch', testDefs, 'color', 'green');
    const result = loadSettings('test-sketch', testDefs);
    expect(result).toEqual({ speed: 1, color: 'green' });
  });
});
