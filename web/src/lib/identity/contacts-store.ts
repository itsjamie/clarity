// The local contact list: friends added by code, mirroring
// `clarity_identity::contacts`. Presence is not modeled here — a contact stays
// unconfirmed until the friend shows up on the presence socket.

import type { ExternalStateStore } from '@/hooks/use-session-state';
import { normalizeFriendCode } from './friend-code';

export interface Contact {
  /** Canonical friend code (`clr-XXXX-XXXX`). */
  code: string;
  /** The local, private label for this friend. */
  name: string;
  addedAt: number;
  /** False until the friend is first seen online; presence sets it. */
  confirmed: boolean;
}

export interface ContactsState {
  contacts: readonly Contact[];
}

const CONTACTS_KEY = 'clarity:contacts';

/**
 * How long an unaccepted invite lives. Adding a friend is something both
 * people do in the moment; a contact the other side has not accepted within
 * this window is dropped rather than left waiting forever. Mirrors the
 * server's request TTL, so the invite disappears for both sides on roughly
 * the same clock.
 */
export const INVITE_TTL_MS = 10 * 60 * 1000;

export class ContactsStore implements ExternalStateStore<ContactsState> {
  readonly #listeners = new Set<() => void>();
  readonly #storage: Pick<Storage, 'getItem' | 'setItem'>;
  #state: ContactsState;

  public constructor(storage: Pick<Storage, 'getItem' | 'setItem'> = window.localStorage) {
    this.#storage = storage;
    this.#state = { contacts: loadContacts(storage) };
  }

  public getSnapshot = (): ContactsState => this.#state;

  public subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  /**
   * Adds a friend by code. The code is normalized first; `ownCode` is rejected
   * so a user cannot add themselves. New contacts start unconfirmed.
   */
  public add(code: string, name: string, ownCode: string | null): Contact {
    const normalized = normalizeFriendCode(code);
    if (!normalized) throw new Error('That is not a valid friend code.');
    if (normalized === ownCode) throw new Error('That is your own code.');
    if (this.#state.contacts.some((contact) => contact.code === normalized)) {
      throw new Error('That friend is already added.');
    }
    const contact: Contact = {
      code: normalized,
      name: name.trim(),
      addedAt: Date.now(),
      confirmed: false,
    };
    this.#replace([...this.#state.contacts, contact]);
    return contact;
  }

  public remove(code: string): void {
    this.#replace(this.#state.contacts.filter((contact) => contact.code !== code));
  }

  /** Marks a contact as confirmed (seen online at least once). */
  public confirm(code: string): void {
    if (!this.#state.contacts.some((contact) => contact.code === code && !contact.confirmed)) {
      return;
    }
    this.#replace(
      this.#state.contacts.map((contact) =>
        contact.code === code ? { ...contact, confirmed: true } : contact,
      ),
    );
  }

  /**
   * Drops unconfirmed contacts whose invite has aged out ([`INVITE_TTL_MS`]);
   * `true` when any were dropped. Removing the contact shrinks the presence
   * subscription, which withdraws the request server-side.
   */
  public expireInvites(now: number = Date.now()): boolean {
    const kept = this.#state.contacts.filter(
      (contact) => contact.confirmed || now - contact.addedAt < INVITE_TTL_MS,
    );
    if (kept.length === this.#state.contacts.length) return false;
    this.#replace(kept);
    return true;
  }

  /** The local name for a friend code, if it is a contact. */
  public nameOf(code: string): string | null {
    return this.#state.contacts.find((contact) => contact.code === code)?.name ?? null;
  }

  #replace(contacts: Contact[]): void {
    this.#state = { contacts };
    this.#storage.setItem(CONTACTS_KEY, JSON.stringify(contacts));
    this.#listeners.forEach((listener) => listener());
  }
}

function loadContacts(storage: Pick<Storage, 'getItem'>): Contact[] {
  try {
    const raw = storage.getItem(CONTACTS_KEY);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(isContact);
  } catch {
    return [];
  }
}

function isContact(value: unknown): value is Contact {
  return (
    typeof value === 'object' &&
    value !== null &&
    typeof (value as Contact).code === 'string' &&
    typeof (value as Contact).name === 'string' &&
    typeof (value as Contact).addedAt === 'number' &&
    typeof (value as Contact).confirmed === 'boolean'
  );
}
