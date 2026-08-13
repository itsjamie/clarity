import { DismissedRequestsStore, openRequests } from './dismissed-requests';

describe('dismissed requests store', () => {
  it('records dismissals once and persists them across reloads', () => {
    const storage = memoryStorage();
    const store = new DismissedRequestsStore(storage);
    expect(store.has('clr-JOYG-7DSO')).toBe(false);

    store.dismiss('clr-JOYG-7DSO');
    store.dismiss('clr-JOYG-7DSO');
    expect(store.getSnapshot().codes).toEqual(['clr-JOYG-7DSO']);

    const reloaded = new DismissedRequestsStore(storage);
    expect(reloaded.has('clr-JOYG-7DSO')).toBe(true);
  });

  it('notifies subscribers on a new dismissal only', () => {
    const store = new DismissedRequestsStore(memoryStorage());
    const listener = vi.fn();
    store.subscribe(listener);
    store.dismiss('clr-JOYG-7DSO');
    store.dismiss('clr-JOYG-7DSO');
    expect(listener).toHaveBeenCalledTimes(1);
  });

  it('ignores corrupt persisted state', () => {
    const storage = memoryStorage();
    storage.setItem('clarity:dismissed-requests', '{nonsense');
    expect(new DismissedRequestsStore(storage).getSnapshot().codes).toEqual([]);
  });
});

describe('openRequests', () => {
  it('filters requests that are already contacts or dismissed', () => {
    const requests = ['clr-JOYG-7DSO', 'clr-RQGM-C6QE', 'clr-AAAA-AAAA'];
    const contacts = [{ code: 'clr-RQGM-C6QE' }];
    const dismissed = ['clr-AAAA-AAAA'];
    expect(openRequests(requests, contacts, dismissed)).toEqual(['clr-JOYG-7DSO']);
  });
});

function memoryStorage(): Pick<Storage, 'getItem' | 'setItem'> {
  const values = new Map<string, string>();
  return {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
  };
}
