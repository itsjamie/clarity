export const storageKeys = {
  presenterSecret: (roomId: string) => `clarity:room:${roomId}:presenter-secret`,
  viewerSecret: (roomId: string) => `clarity:room:${roomId}:viewer-secret`,
  viewerUrl: (roomId: string) => `clarity:room:${roomId}:viewer-url`,
  resumeToken: (roomId: string, role: 'presenter' | 'viewer') =>
    `clarity:room:${roomId}:${role}:resume-token`,
  syntheticCapture: 'clarity:test:synthetic-capture',
} as const;

export function storeSessionSecret(key: string, value: string): void {
  window.sessionStorage.setItem(key, value);
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
