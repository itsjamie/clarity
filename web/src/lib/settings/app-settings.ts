// App-wide settings shared between features. Stored as one JSON object under
// `clarity:settings` in localStorage; the settings page (and anything else)
// reads and writes through this module so the key layout stays a contract.

import type { CaptureMode } from '@/lib/webrtc/profiles';
import {
  DEFAULT_CAPTURE_RESOLUTION,
  type CaptureResolution,
} from '@/lib/media/capture-resolution';

export interface AppSettings {
  /**
   * Route all WebRTC media through the TURN relay (`iceTransportPolicy:
   * 'relay'`), hiding this browser's IP from peers at the cost of latency.
   */
  forceRelay: boolean;
  /** Capture profile new presenter sessions start with. */
  captureMode: CaptureMode;
  /** Resolution ceiling new presenter sessions request from the browser. */
  captureResolution: CaptureResolution;
  /** Whether new presenter sessions ask for system audio with the capture. */
  captureAudio: boolean;
}

export const APP_SETTINGS_STORAGE_KEY = 'clarity:settings';

const DEFAULT_SETTINGS: AppSettings = {
  forceRelay: false,
  captureMode: 'text',
  captureResolution: DEFAULT_CAPTURE_RESOLUTION,
  captureAudio: true,
};

type SettingsStorage = Pick<Storage, 'getItem' | 'setItem'>;

function browserStorage(): SettingsStorage | null {
  try {
    return window.localStorage ?? null;
  } catch {
    return null;
  }
}

function isCaptureMode(value: unknown): value is CaptureMode {
  return value === 'text' || value === 'motion';
}

function isCaptureResolution(value: unknown): value is CaptureResolution {
  return value === '1440p' || value === '4k';
}

export function loadAppSettings(
  storage: SettingsStorage | null = browserStorage(),
): AppSettings {
  const raw = storage?.getItem(APP_SETTINGS_STORAGE_KEY);
  if (!raw) return { ...DEFAULT_SETTINGS };
  try {
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== 'object' || parsed === null) return { ...DEFAULT_SETTINGS };
    const candidate = parsed as Record<string, unknown>;
    return {
      forceRelay: typeof candidate.forceRelay === 'boolean'
        ? candidate.forceRelay
        : DEFAULT_SETTINGS.forceRelay,
      captureMode: isCaptureMode(candidate.captureMode)
        ? candidate.captureMode
        : DEFAULT_SETTINGS.captureMode,
      captureResolution: isCaptureResolution(candidate.captureResolution)
        ? candidate.captureResolution
        : DEFAULT_SETTINGS.captureResolution,
      captureAudio: typeof candidate.captureAudio === 'boolean'
        ? candidate.captureAudio
        : DEFAULT_SETTINGS.captureAudio,
    };
  } catch {
    return { ...DEFAULT_SETTINGS };
  }
}

export function saveAppSettings(
  patch: Partial<AppSettings>,
  storage: SettingsStorage | null = browserStorage(),
): AppSettings {
  const next = { ...loadAppSettings(storage), ...patch };
  storage?.setItem(APP_SETTINGS_STORAGE_KEY, JSON.stringify(next));
  return next;
}

/**
 * Whether peer connections must use `iceTransportPolicy: 'relay'`. True when
 * the user setting is on, or when the Playwright force-relay test hook is
 * active in test builds.
 */
export function forceRelayEnabled(): boolean {
  if (
    import.meta.env.MODE === 'test' &&
    window.sessionStorage.getItem('clarity:test:force-relay') === 'enabled'
  ) {
    return true;
  }
  return loadAppSettings().forceRelay;
}
