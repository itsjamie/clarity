// Friend codes whose incoming requests were dismissed. The server pushes the
// full pending set on every connect, so without this record a dismissed
// request would re-nag forever. Dismissal is local only; the requester is not
// told.

import type { ExternalStateStore } from '@/hooks/use-session-state';

export interface DismissedRequestsState {
  codes: readonly string[];
}

const DISMISSED_KEY = 'clarity:dismissed-requests';

export class DismissedRequestsStore implements ExternalStateStore<DismissedRequestsState> {
  readonly #listeners = new Set<() => void>();
  readonly #storage: Pick<Storage, 'getItem' | 'setItem'>;
  #state: DismissedRequestsState;

  public constructor(storage: Pick<Storage, 'getItem' | 'setItem'> = window.localStorage) {
    this.#storage = storage;
    this.#state = { codes: loadDismissed(storage) };
  }

  public getSnapshot = (): DismissedRequestsState => this.#state;

  public subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  public has(code: string): boolean {
    return this.#state.codes.includes(code);
  }

  public dismiss(code: string): void {
    if (this.has(code)) return;
    this.#state = { codes: [...this.#state.codes, code] };
    this.#storage.setItem(DISMISSED_KEY, JSON.stringify(this.#state.codes));
    this.#listeners.forEach((listener) => listener());
  }
}

/**
 * Incoming friend requests still needing an answer: reported by the server,
 * not already a contact, and not dismissed earlier.
 */
export function openRequests(
  requests: readonly string[],
  contacts: readonly { code: string }[],
  dismissed: readonly string[],
): string[] {
  return requests.filter(
    (code) =>
      !contacts.some((contact) => contact.code === code) && !dismissed.includes(code),
  );
}

function loadDismissed(storage: Pick<Storage, 'getItem'>): string[] {
  try {
    const raw = storage.getItem(DISMISSED_KEY);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((value): value is string => typeof value === 'string');
  } catch {
    return [];
  }
}
