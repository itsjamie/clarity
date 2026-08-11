import type { CaptureMode } from '@/lib/webrtc/profiles';

export const storageKeys = {
  presenterSecret: (roomId: string) => `clarity:room:${roomId}:presenter-secret`,
  viewerSecret: (roomId: string) => `clarity:room:${roomId}:viewer-secret`,
  viewerUrl: (roomId: string) => `clarity:room:${roomId}:viewer-url`,
  captureMode: (roomId: string) => `clarity:room:${roomId}:capture-mode`,
  resumeToken: (roomId: string, role: 'presenter' | 'viewer') =>
    `clarity:room:${roomId}:${role}:resume-token`,
  syntheticCapture: 'clarity:test:synthetic-capture',
} as const;

export function storeSessionSecret(key: string, value: string): void {
  window.sessionStorage.setItem(key, value);
}

export interface PresenterCredentials {
  presenterSecret: string;
  viewerUrl: string;
}

/**
 * Persists presenter credentials in localStorage (keyed by room) so the
 * presenter can rejoin an open room from a new tab, and mirrors them into
 * sessionStorage for the current tab.
 */
export function storePresenterCredentials(
  roomId: string,
  credentials: PresenterCredentials,
): void {
  for (const storage of credentialStorages()) {
    storage.setItem(storageKeys.presenterSecret(roomId), credentials.presenterSecret);
    storage.setItem(storageKeys.viewerUrl(roomId), credentials.viewerUrl);
  }
}

export function loadPresenterCredentials(roomId: string): PresenterCredentials | null {
  for (const storage of credentialStorages()) {
    const presenterSecret = storage.getItem(storageKeys.presenterSecret(roomId));
    const viewerUrl = storage.getItem(storageKeys.viewerUrl(roomId));
    if (presenterSecret && viewerUrl) return { presenterSecret, viewerUrl };
  }
  return null;
}

/** Forgets a room's presenter credentials everywhere (room closed or expired). */
export function clearPresenterCredentials(roomId: string): void {
  for (const storage of credentialStorages()) {
    storage.removeItem(storageKeys.presenterSecret(roomId));
    storage.removeItem(storageKeys.viewerUrl(roomId));
    storage.removeItem(storageKeys.captureMode(roomId));
  }
}

/**
 * Remembers the capture profile chosen when the room was created so the
 * presenter session starts with it, even after a rejoin from another tab.
 */
export function storeRoomCaptureMode(roomId: string, mode: CaptureMode): void {
  for (const storage of credentialStorages()) {
    storage.setItem(storageKeys.captureMode(roomId), mode);
  }
}

export function loadRoomCaptureMode(roomId: string): CaptureMode | null {
  for (const storage of credentialStorages()) {
    const mode = storage.getItem(storageKeys.captureMode(roomId));
    if (mode === 'text' || mode === 'motion') return mode;
  }
  return null;
}

function credentialStorages(): Storage[] {
  const storages: Array<Storage | undefined> = [window.sessionStorage, window.localStorage];
  return storages.filter((storage): storage is Storage => storage !== undefined);
}

export function takeInviteSecret(
  roomId: string,
  location: Pick<Location, 'hash' | 'pathname' | 'search'> = window.location,
  storage: Pick<Storage, 'getItem' | 'setItem'> = window.sessionStorage,
  history: Pick<History, 'replaceState'> = window.history,
): string | null {
  const key = storageKeys.viewerSecret(roomId);
  const fragment = location.hash.startsWith('#')
    ? decodeURIComponent(location.hash.slice(1))
    : '';
  if (fragment) {
    storage.setItem(key, fragment);
    history.replaceState(null, '', `${location.pathname}${location.search}`);
    return fragment;
  }
  return storage.getItem(key);
}
