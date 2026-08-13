// The presence WebSocket client (`/api/v1/presence`). Authenticated by
// identity rather than a room secret: the server issues a challenge, the
// client signs it with its Ed25519 key, and from then on subscribes to its
// contacts and announces what it is hosting.

import type {
  FriendPresence,
  HostedRoom,
  PresenceClientMessage,
  PresenceServerMessage,
} from '@/generated/protocol';
import { PROTOCOL_VERSION } from '@/config/environment';
import { identityChallengePayload } from '@/lib/identity/identity-challenge';
import { reconnectDelay } from '@/lib/signaling/signaling-client';
import { bytesToBase64 } from '@/lib/identity/identity-store';
import type { ExternalStateStore } from '@/hooks/use-session-state';

export type PresenceStatus =
  | 'idle'
  | 'connecting'
  | 'authenticating'
  | 'ready'
  | 'reconnecting'
  | 'closed'
  | 'failed';

export interface PresenceIdentity {
  publicKeyBase64: string;
  sign(message: Uint8Array): Promise<Uint8Array>;
}

export interface PresenceState {
  status: PresenceStatus;
  /** The friend code the server derived from our public key. */
  selfCode: string | null;
  friends: readonly FriendPresence[];
  /**
   * Codes that added this identity and are waiting for it to add them back —
   * incoming friend requests, as the server last reported them.
   */
  requests: readonly string[];
}

/**
 * A hosting announcement: the room to show friends plus the presenter secret
 * that proves this session hosts it. The secret goes only to the server,
 * which drops unproven announcements; it is never forwarded to friends.
 */
export interface HostingAnnouncement {
  room: HostedRoom;
  presenterSecret: string;
}

interface PresenceClientOptions {
  url: string;
  identity: PresenceIdentity;
  webSocketFactory?: (url: string) => WebSocket;
}

export class PresenceClient implements ExternalStateStore<PresenceState> {
  readonly #options: PresenceClientOptions;
  readonly #webSocketFactory: (url: string) => WebSocket;
  readonly #listeners = new Set<() => void>();
  #socket: WebSocket | null = null;
  #stopped = true;
  #attempt = 0;
  #reconnectTimer: number | null = null;
  #contactCodes: readonly string[] = [];
  #hosting: HostingAnnouncement | null = null;
  #state: PresenceState = { status: 'idle', selfCode: null, friends: [], requests: [] };

  public constructor(options: PresenceClientOptions) {
    this.#options = options;
    this.#webSocketFactory = options.webSocketFactory ?? ((url) => new WebSocket(url));
  }

  public getSnapshot = (): PresenceState => this.#state;

  public subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  public connect(): void {
    if (!this.#stopped) return;
    this.#stopped = false;
    this.#open(false);
  }

  public disconnect(): void {
    this.#stopped = true;
    if (this.#reconnectTimer !== null) {
      window.clearTimeout(this.#reconnectTimer);
      this.#reconnectTimer = null;
    }
    this.#socket?.close(1000, 'presence ended');
    this.#socket = null;
    this.#patch({ status: 'closed' });
  }

  /**
   * Replaces the watched contact set. Sent immediately when the session is
   * ready and replayed after every reconnect.
   */
  public setContacts(codes: readonly string[]): void {
    this.#contactCodes = [...codes];
    this.#sendWhenReady({
      type: 'presence:subscribe',
      protocolVersion: PROTOCOL_VERSION,
      codes: [...codes],
    });
  }

  /**
   * Announces the room this identity is hosting, or `null` when it stops.
   * Sticky: the latest announcement is replayed after every reconnect.
   */
  public announce(hosting: HostingAnnouncement | null): void {
    this.#hosting = hosting;
    this.#sendWhenReady(announceMessage(hosting));
  }

  #open(reconnecting: boolean): void {
    this.#patch({ status: reconnecting ? 'reconnecting' : 'connecting' });
    const socket = this.#webSocketFactory(this.#options.url);
    this.#socket = socket;
    socket.addEventListener('open', () => {
      if (this.#socket !== socket) {
        socket.close(1000, 'superseded');
        return;
      }
      this.#patch({ status: 'authenticating' });
    });
    socket.addEventListener('message', (event: MessageEvent<unknown>) => {
      if (this.#socket !== socket) return;
      if (typeof event.data !== 'string') return;
      let message: PresenceServerMessage;
      try {
        message = parsePresenceMessage(event.data);
      } catch {
        return;
      }
      void this.#handleMessage(socket, message);
    });
    socket.addEventListener('close', () => {
      if (this.#socket !== socket) return;
      this.#socket = null;
      if (!this.#stopped) this.#scheduleReconnect();
    });
    socket.addEventListener('error', () => {
      if (this.#socket === socket) socket.close();
    });
  }

  async #handleMessage(socket: WebSocket, message: PresenceServerMessage): Promise<void> {
    switch (message.type) {
      case 'presence:challenge': {
        const payload = identityChallengePayload('presence', this.#options.url, message.nonce);
        const signature = await this.#options.identity.sign(
          new TextEncoder().encode(payload),
        );
        if (this.#socket !== socket || socket.readyState !== WebSocket.OPEN) return;
        this.#sendOn(socket, {
          type: 'presence:hello',
          protocolVersion: PROTOCOL_VERSION,
          publicKey: this.#options.identity.publicKeyBase64,
          signature: bytesToBase64(signature),
        });
        break;
      }
      case 'presence:ready':
        this.#attempt = 0;
        this.#patch({ status: 'ready', selfCode: message.code });
        this.#sendOn(socket, {
          type: 'presence:subscribe',
          protocolVersion: PROTOCOL_VERSION,
          codes: [...this.#contactCodes],
        });
        if (this.#hosting) {
          this.#sendOn(socket, announceMessage(this.#hosting));
        }
        break;
      case 'presence:snapshot':
        this.#patch({ friends: sortFriends(message.friends) });
        break;
      case 'presence:requests':
        this.#patch({ requests: [...message.codes] });
        break;
      case 'presence:update':
        this.#patch({
          friends: sortFriends([
            ...this.#state.friends.filter((friend) => friend.code !== message.friend.code),
            message.friend,
          ]),
        });
        break;
      case 'error':
        if (
          message.code === 'authentication_failed' ||
          message.code === 'unsupported_protocol_version'
        ) {
          this.#stopped = true;
          this.#socket = null;
          socket.close();
          this.#patch({ status: 'failed' });
        }
        break;
    }
  }

  #sendWhenReady(message: PresenceClientMessage): void {
    if (this.#state.status !== 'ready' || this.#socket?.readyState !== WebSocket.OPEN) {
      return; // Replayed from the sticky fields once presence:ready arrives.
    }
    this.#sendOn(this.#socket, message);
  }

  #sendOn(socket: WebSocket, message: PresenceClientMessage): void {
    socket.send(JSON.stringify(message));
  }

  #scheduleReconnect(): void {
    this.#patch({ status: 'reconnecting' });
    const delay = reconnectDelay(this.#attempt, secureJitter());
    this.#attempt += 1;
    this.#reconnectTimer = window.setTimeout(() => {
      this.#reconnectTimer = null;
      if (!this.#stopped) this.#open(true);
    }, delay);
  }

  #patch(patch: Partial<PresenceState>): void {
    this.#state = { ...this.#state, ...patch };
    this.#listeners.forEach((listener) => listener());
  }
}

const PRESENCE_MESSAGE_TYPES: ReadonlySet<string> = new Set([
  'presence:challenge',
  'presence:ready',
  'presence:snapshot',
  'presence:update',
  'presence:requests',
  'error',
]);

function parsePresenceMessage(input: string): PresenceServerMessage {
  const value: unknown = JSON.parse(input);
  if (
    typeof value !== 'object' ||
    value === null ||
    !PRESENCE_MESSAGE_TYPES.has((value as { type?: unknown }).type as string)
  ) {
    throw new Error('The server sent an invalid presence message.');
  }
  return value as PresenceServerMessage;
}

function announceMessage(hosting: HostingAnnouncement | null): PresenceClientMessage {
  return {
    type: 'presence:announce',
    protocolVersion: PROTOCOL_VERSION,
    hosting: hosting?.room ?? null,
    presenterSecret: hosting?.presenterSecret ?? null,
  };
}

function sortFriends(friends: FriendPresence[]): FriendPresence[] {
  return [...friends].sort((a, b) => a.code.localeCompare(b.code));
}

function secureJitter(): number {
  const value = new Uint32Array(1);
  crypto.getRandomValues(value);
  return (value[0] ?? 0) / 0xffff_ffff;
}
