import {
  APP_SETTINGS_STORAGE_KEY,
  loadAppSettings,
  saveAppSettings,
} from './app-settings';

class FakeStorage {
  #entries = new Map<string, string>();
  getItem = (key: string): string | null => this.#entries.get(key) ?? null;
  setItem = (key: string, value: string): void => {
    this.#entries.set(key, value);
  };
}

describe('app settings', () => {
  it('returns the defaults when nothing is stored', () => {
    expect(loadAppSettings(new FakeStorage())).toEqual({
      forceRelay: false,
      captureMode: 'text',
      captureResolution: '1440p',
      captureAudio: true,
    });
  });

  it('round-trips a partial patch on top of the stored settings', () => {
    const storage = new FakeStorage();
    saveAppSettings({ captureMode: 'motion', forceRelay: true }, storage);
    saveAppSettings({ captureResolution: '4k' }, storage);
    expect(loadAppSettings(storage)).toEqual({
      forceRelay: true,
      captureMode: 'motion',
      captureResolution: '4k',
      captureAudio: true,
    });
  });

  it('falls back per field when the stored value is invalid', () => {
    const storage = new FakeStorage();
    storage.setItem(
      APP_SETTINGS_STORAGE_KEY,
      JSON.stringify({ captureMode: 'ultra', captureResolution: '8k', captureAudio: 'yes', forceRelay: true }),
    );
    expect(loadAppSettings(storage)).toEqual({
      forceRelay: true,
      captureMode: 'text',
      captureResolution: '1440p',
      captureAudio: true,
    });
  });

  it('survives corrupted JSON', () => {
    const storage = new FakeStorage();
    storage.setItem(APP_SETTINGS_STORAGE_KEY, '{not json');
    expect(loadAppSettings(storage)).toEqual({
      forceRelay: false,
      captureMode: 'text',
      captureResolution: '1440p',
      captureAudio: true,
    });
  });
});
