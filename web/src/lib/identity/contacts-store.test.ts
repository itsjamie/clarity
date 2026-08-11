import { ContactsStore } from './contacts-store';

describe('contacts store', () => {
  it('normalizes codes and starts contacts unconfirmed', () => {
    const store = new ContactsStore(memoryStorage());
    const contact = store.add('  joyg 7dso ', ' Mara ', 'clr-AAAA-AAAA');
    expect(contact.code).toBe('clr-JOYG-7DSO');
    expect(contact.name).toBe('Mara');
    expect(contact.confirmed).toBe(false);
    expect(store.nameOf('clr-JOYG-7DSO')).toBe('Mara');
  });

  it('rejects invalid codes, the own code, and duplicates', () => {
    const store = new ContactsStore(memoryStorage());
    expect(() => store.add('clr-abc', 'X', null)).toThrow('not a valid friend code');
    expect(() => store.add('clr-JOYG-7DSO', 'Me', 'clr-JOYG-7DSO')).toThrow('your own code');
    store.add('clr-JOYG-7DSO', 'Mara', null);
    expect(() => store.add('joyg7dso', 'Again', null)).toThrow('already added');
  });

  it('confirms, removes, and persists across reloads', () => {
    const storage = memoryStorage();
    const store = new ContactsStore(storage);
    store.add('clr-JOYG-7DSO', 'Mara', null);
    store.add('clr-RQGM-C6QE', 'Dan', null);
    store.confirm('clr-JOYG-7DSO');
    store.remove('clr-RQGM-C6QE');

    const reloaded = new ContactsStore(storage);
    expect(reloaded.getSnapshot().contacts).toHaveLength(1);
    expect(reloaded.getSnapshot().contacts[0]).toMatchObject({
      code: 'clr-JOYG-7DSO',
      name: 'Mara',
      confirmed: true,
    });
  });

  it('notifies subscribers and keeps snapshots immutable per change', () => {
    const store = new ContactsStore(memoryStorage());
    const listener = vi.fn();
    store.subscribe(listener);
    const before = store.getSnapshot();
    store.add('clr-JOYG-7DSO', 'Mara', null);
    expect(listener).toHaveBeenCalledTimes(1);
    expect(store.getSnapshot()).not.toBe(before);
  });

  it('ignores corrupt persisted state', () => {
    const storage = memoryStorage();
    storage.setItem('clarity:contacts', '{nonsense');
    expect(new ContactsStore(storage).getSnapshot().contacts).toEqual([]);
  });
});

function memoryStorage(): Pick<Storage, 'getItem' | 'setItem'> {
  const values = new Map<string, string>();
  return {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
  };
}
