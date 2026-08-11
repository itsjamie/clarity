import { webcrypto } from 'node:crypto';

import {
  IdentityStore,
  type IdentityKeyStorage,
  type StoredIdentityKeys,
} from './identity-store';

const subtle = webcrypto.subtle as SubtleCrypto;

describe('identity store', () => {
  it('creates a ready identity with a friend code and signing key', async () => {
    const store = new IdentityStore({ keys: memoryKeys(), storage: memoryStorage(), subtle });
    await store.load();
    expect(store.getSnapshot().status).toBe('absent');

    await store.create('Jamie', 'Test device');
    const state = store.getSnapshot();
    expect(state.status).toBe('ready');
    expect(state.displayName).toBe('Jamie');
    expect(state.deviceLabel).toBe('Test device');
    expect(state.friendCode).toMatch(/^clr-[A-Z2-7]{4}-[A-Z2-7]{4}$/);
    expect(atob(state.publicKeyBase64!)).toHaveLength(32);

    const signature = await store.sign(new TextEncoder().encode('challenge-nonce'));
    expect(signature).toHaveLength(64);
  });

  it('restores a persisted identity on load', async () => {
    const keys = memoryKeys();
    const storage = memoryStorage();
    const first = new IdentityStore({ keys, storage, subtle });
    await first.create('Jamie');
    const code = first.getSnapshot().friendCode;

    const second = new IdentityStore({ keys, storage, subtle });
    await second.load();
    expect(second.getSnapshot().status).toBe('ready');
    expect(second.getSnapshot().friendCode).toBe(code);
    expect(second.getSnapshot().displayName).toBe('Jamie');
  });

  it('rotates to a new friend code while keeping the names', async () => {
    const store = new IdentityStore({ keys: memoryKeys(), storage: memoryStorage(), subtle });
    await store.create('Jamie', 'Studio');
    const before = store.getSnapshot().friendCode;
    await store.rotate();
    const after = store.getSnapshot();
    expect(after.friendCode).not.toBe(before);
    expect(after.displayName).toBe('Jamie');
    expect(after.deviceLabel).toBe('Studio');
  });

  it('reports unsupported when Ed25519 keys cannot be generated', async () => {
    const noEd25519 = {
      generateKey: () => Promise.reject(new Error('unsupported algorithm')),
    } as unknown as SubtleCrypto;
    const store = new IdentityStore({
      keys: memoryKeys(),
      storage: memoryStorage(),
      subtle: noEd25519,
    });
    await store.load();
    expect(store.getSnapshot().status).toBe('unsupported');
    await expect(store.create('Jamie')).rejects.toThrow();
    expect(store.getSnapshot().status).toBe('unsupported');
  });

  it('persists edited names', () => {
    const storage = memoryStorage();
    const store = new IdentityStore({ keys: memoryKeys(), storage, subtle });
    store.setDisplayName('  Mara  ');
    store.setDeviceLabel('Laptop');
    expect(storage.getItem('clarity:identity:display-name')).toBe('Mara');
    expect(storage.getItem('clarity:identity:device-label')).toBe('Laptop');
  });
});

function memoryKeys(): IdentityKeyStorage {
  let stored: StoredIdentityKeys | null = null;
  return {
    load: () => Promise.resolve(stored),
    save: (keys) => {
      stored = keys;
      return Promise.resolve();
    },
  };
}

function memoryStorage(): Pick<Storage, 'getItem' | 'setItem' | 'removeItem'> {
  const values = new Map<string, string>();
  return {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
    removeItem: (key) => values.delete(key),
  };
}
