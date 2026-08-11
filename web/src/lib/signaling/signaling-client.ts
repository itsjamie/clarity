import type { ClientMessage, PeerRole, ServerMessage } from '@/generated/protocol';
import { PROTOCOL_VERSION } from '@/config/environment';
import { identityChallengePayload } from '@/lib/identity/identity-challenge';
import { parseServerMessage } from '@/lib/protocol/validate-message';
import { storageKeys } from '@/lib/storage/session-storage';

export type SignalingState =
  | 'idle'
  | 'connecting'
  | 'authenticating'
  | 'connected'
  | 'reconnecting'
  | 'closed'
  | 'failed';

/**
 * Signs a friends-only identity challenge. The bytes are the domain-separated
 * challenge payload (see `identityChallengePayload`), not the raw nonce.
 * Resolves `null` when no identity is available, in which case the challenge
 * goes unanswered and the server fails the authentication.
 */
export type SignalingIdentityProvider = (
  payload: Uint8Array,
) => Promise<{ publicKey: string; signature: string } | null>;

interface SignalingClientOptions {
  url: string;
  roomId: string;
  role: PeerRole;
  authentication: ClientMessage;
  onMessage: (message: ServerMessage) => void;
  onStateChange: (state: SignalingState) => void;
  identity?: SignalingIdentityProvider;
  storage?: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
  webSocketFactory?: (url: string) => WebSocket;
}

export class SignalingClient {
  readonly #options: SignalingClientOptions;
  readonly #storage: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
  readonly #webSocketFactory: (url: string) => WebSocket;
  #socket: WebSocket | null = null;
  #stopped = true;
  #attempt = 0;
  #reconnectTimer: number | null = null;
  #state: SignalingState = 'idle';

  public constructor(options: SignalingClientOptions) {
    this.#options = options;
    this.#storage = options.storage ?? window.sessionStorage;
    this.#webSocketFactory = options.webSocketFactory ?? ((url) => new WebSocket(url));
  }

  public get state(): SignalingState {
    return this.#state;
  }

  public connect(): void {
    if (!this.#stopped) return;
    this.#stopped = false;
    this.#open(false);
  }

  public send(message: ClientMessage): void {
    if (this.#socket?.readyState !== WebSocket.OPEN) {
      throw new Error('Signaling is not connected.');
    }
    this.#socket.send(JSON.stringify(message));
  }

  public disconnect(sendLeave = true): void {
    this.#stopped = true;
    if (this.#reconnectTimer !== null) {
      window.clearTimeout(this.#reconnectTimer);
      this.#reconnectTimer = null;
    }
    if (sendLeave && this.#socket?.readyState === WebSocket.OPEN) {
      this.send({
        type: 'peer:leave',
        protocolVersion: PROTOCOL_VERSION,
        requestId: crypto.randomUUID(),
      });
    }
    this.#socket?.close(1000, 'session ended');
    this.#socket = null;
    this.#setState('closed');
  }

  #open(reconnecting: boolean): void {
    this.#setState(reconnecting ? 'reconnecting' : 'connecting');
    const socket = this.#webSocketFactory(this.#options.url);
    this.#socket = socket;
    let resumeAttempted = false;
    socket.addEventListener('open', () => {
      if (this.#socket !== socket) {
        socket.close(1000, 'superseded');
        return;
      }
      this.#setState('authenticating');
      const token = this.#storage.getItem(
        storageKeys.resumeToken(this.#options.roomId, this.#options.role),
      );
      resumeAttempted = token !== null;
      const authentication: ClientMessage = token
        ? {
            type: 'session:resume',
            protocolVersion: PROTOCOL_VERSION,
            requestId: crypto.randomUUID(),
            roomId: this.#options.roomId,
            resumeToken: token,
          }
        : this.#options.authentication;
      socket.send(JSON.stringify(authentication));
    });
    socket.addEventListener('message', (event: MessageEvent<unknown>) => {
      if (this.#socket !== socket) return;
      if (typeof event.data !== 'string') {
        this.#fail();
        return;
      }
      try {
        const message = parseServerMessage(event.data);
        if (message.type === 'heartbeat:ping') {
          this.send({
            type: 'heartbeat:pong',
            protocolVersion: PROTOCOL_VERSION,
            nonce: message.nonce,
          });
          return;
        }
        if (message.type === 'auth:identity-challenge') {
          void this.#answerIdentityChallenge(socket, message.requestId, message.nonce);
          this.#options.onMessage(message);
          return;
        }
        if (message.type === 'auth:succeeded') {
          this.#storage.setItem(
            storageKeys.resumeToken(this.#options.roomId, this.#options.role),
            message.resumeToken,
          );
          this.#attempt = 0;
          this.#setState('connected');
        } else if (message.type === 'auth:failed') {
          this.#storage.removeItem(
            storageKeys.resumeToken(this.#options.roomId, this.#options.role),
          );
          if (resumeAttempted && !this.#stopped) {
            // The resume was rejected (expired grace, restarted server, …).
            // The token is gone now, so a fresh connection re-runs the
            // original authentication instead of surfacing a failure.
            socket.close(1000, 'resume rejected');
            this.#socket = null;
            this.#open(true);
            return;
          }
          // The original authentication was definitively rejected (bad
          // secret, not on a friends-only allowlist, …). Reconnecting would
          // re-run the identity challenge into the same rejection forever,
          // so the client stops; recovery is a fresh connect().
          this.#stopped = true;
          this.#setState('failed');
        }
        this.#options.onMessage(message);
      } catch {
        this.#fail();
      }
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

  async #answerIdentityChallenge(socket: WebSocket, requestId: string, nonce: string): Promise<void> {
    if (!this.#options.identity) return;
    let proof: { publicKey: string; signature: string } | null = null;
    try {
      const payload = identityChallengePayload('room-auth', this.#options.url, nonce);
      proof = await this.#options.identity(new TextEncoder().encode(payload));
    } catch {
      proof = null;
    }
    if (!proof || this.#socket !== socket || socket.readyState !== WebSocket.OPEN) return;
    socket.send(
      JSON.stringify({
        type: 'auth:identity',
        protocolVersion: PROTOCOL_VERSION,
        requestId,
        publicKey: proof.publicKey,
        signature: proof.signature,
      } satisfies ClientMessage),
    );
  }

  #scheduleReconnect(): void {
    this.#setState('reconnecting');
    const delay = reconnectDelay(this.#attempt, secureJitter());
    this.#attempt += 1;
    this.#reconnectTimer = window.setTimeout(() => {
      this.#reconnectTimer = null;
      if (!this.#stopped) this.#open(true);
    }, delay);
  }

  #fail(): void {
    this.#setState('failed');
    this.#socket?.close(1002, 'protocol error');
  }

  #setState(state: SignalingState): void {
    if (state === this.#state) return;
    this.#state = state;
    this.#options.onStateChange(state);
  }
}

export function reconnectDelay(attempt: number, jitter: number): number {
  const base = Math.min(500 * 2 ** Math.min(attempt, 5), 10_000);
  return Math.round(base * (0.75 + Math.max(0, Math.min(jitter, 1)) * 0.5));
}

function secureJitter(): number {
  const value = new Uint32Array(1);
  crypto.getRandomValues(value);
  return (value[0] ?? 0) / 0xffff_ffff;
}
