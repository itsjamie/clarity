import { storageKeys, takeInviteSecret } from './session-storage';

describe('viewer invitation storage', () => {
  it('extracts the fragment, stores it in session storage, and removes it from the address bar', () => {
    const values = new Map<string, string>();
    const replaceState = vi.fn();
    const secret = takeInviteSecret(
      'room',
      { hash: '#s3cret', pathname: '/r/room', search: '' },
      {
        getItem: (key) => values.get(key) ?? null,
        setItem: (key, value) => values.set(key, value),
      },
      { replaceState },
    );
    expect(secret).toBe('s3cret');
    expect(values.get(storageKeys.viewerSecret('room'))).toBe('s3cret');
    expect(replaceState).toHaveBeenCalledWith(null, '', '/r/room');
  });

  it('recovers the secret from session storage when no fragment is present', () => {
    const key = storageKeys.viewerSecret('room');
    const secret = takeInviteSecret(
      'room',
      { hash: '', pathname: '/r/room', search: '' },
      { getItem: (candidate) => (candidate === key ? 'stored' : null), setItem: vi.fn() },
      { replaceState: vi.fn() },
    );
    expect(secret).toBe('stored');
  });
});
