// The signaling server this browser talks to. The web app is served by its
// own server, so the default is always same-origin; the stored value exists
// so the first-run screen and Settings agree with the desktop client's shape.

const SERVER_URL_KEY = 'clarity:settings:server-url';

export function defaultServerUrl(): string {
  return window.location.origin;
}

export function loadServerUrl(
  storage: Pick<Storage, 'getItem'> = window.localStorage,
): string {
  return storage.getItem(SERVER_URL_KEY) ?? defaultServerUrl();
}

export function saveServerUrl(
  url: string,
  storage: Pick<Storage, 'setItem' | 'removeItem'> = window.localStorage,
): void {
  const trimmed = url.trim();
  if (!trimmed || trimmed === defaultServerUrl()) {
    storage.removeItem(SERVER_URL_KEY);
    return;
  }
  storage.setItem(SERVER_URL_KEY, trimmed);
}
