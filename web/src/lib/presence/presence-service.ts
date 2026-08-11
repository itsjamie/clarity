// App-wide presence wiring. One IdentityStore, one ContactsStore, and one
// PresenceClient serve the whole tab: the shell, the presenter console, and
// the viewer all observe the same stores. The client is created lazily once
// the identity is ready, and `presenceStore` lets components subscribe before
// that happens.

import { presenceUrl } from '@/config/environment';
import { ContactsStore } from '@/lib/identity/contacts-store';
import { IdentityStore } from '@/lib/identity/identity-store';
import type { ExternalStateStore } from '@/hooks/use-session-state';
import {
  PresenceClient,
  type HostingAnnouncement,
  type PresenceState,
} from './presence-client';

export const identityStore = new IdentityStore();
export const contactsStore = new ContactsStore();

const IDLE_PRESENCE: PresenceState = { status: 'idle', selfCode: null, friends: [] };

let client: PresenceClient | null = null;
let clientKey: string | null = null;
let started = false;
let pendingAnnouncement: HostingAnnouncement | null = null;
const listeners = new Set<() => void>();

/** A stable store over the lazily-created presence client. */
export const presenceStore: ExternalStateStore<PresenceState> = {
  subscribe(listener) {
    listeners.add(listener);
    return () => listeners.delete(listener);
  },
  getSnapshot() {
    return client?.getSnapshot() ?? IDLE_PRESENCE;
  },
};

/**
 * Loads the identity and, once it is ready, connects the presence socket,
 * keeps the contact subscription current, and confirms contacts the first
 * time they are seen online. Idempotent; call from any route that needs
 * presence.
 */
export function ensurePresenceStarted(): void {
  if (started) return;
  started = true;
  contactsStore.subscribe(() => client?.setContacts(contactCodes()));
  void identityStore.load().then(() => {
    connectWhenReady();
    identityStore.subscribe(connectWhenReady);
  });
}

/** Announces the hosted room (or `null` when hosting ends) to friends. */
export function announceHosting(hosting: HostingAnnouncement | null): void {
  pendingAnnouncement = hosting;
  client?.announce(hosting);
}

function connectWhenReady(): void {
  const identity = identityStore.getSnapshot();
  if (identity.status !== 'ready' || !identity.publicKeyBase64) return;
  if (clientKey === identity.publicKeyBase64) return;
  client?.disconnect();
  clientKey = identity.publicKeyBase64;
  client = new PresenceClient({
    url: presenceUrl(),
    identity: {
      publicKeyBase64: identity.publicKeyBase64,
      sign: (message) => identityStore.sign(message),
    },
  });
  client.subscribe(() => {
    confirmSeenContacts();
    listeners.forEach((listener) => listener());
  });
  client.setContacts(contactCodes());
  if (pendingAnnouncement) client.announce(pendingAnnouncement);
  client.connect();
  listeners.forEach((listener) => listener());
}

function contactCodes(): string[] {
  return contactsStore.getSnapshot().contacts.map((contact) => contact.code);
}

function confirmSeenContacts(): void {
  const friends = client?.getSnapshot().friends ?? [];
  for (const friend of friends) {
    contactsStore.confirm(friend.code);
  }
}
