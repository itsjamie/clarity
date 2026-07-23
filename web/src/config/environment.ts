export const PROTOCOL_VERSION = 3 as const;

export function signalingUrl(location: Location = window.location): string {
  const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
  return `${protocol}//${location.host}/api/v1/ws`;
}

export function isSyntheticCaptureEnabled(): boolean {
  return (
    import.meta.env.MODE === 'test' &&
    import.meta.env.VITE_ENABLE_SYNTHETIC_CAPTURE === 'true'
  );
}
