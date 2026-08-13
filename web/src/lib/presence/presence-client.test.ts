import type { FriendPresence, PresenceServerMessage } from '@/generated/protocol';
import { PROTOCOL_VERSION } from '@/config/environment';
import { PresenceClient, type PresenceIdentity } from './presence-client';

describe('presence client', () => {
  it('answers the challenge, resubscribes, and streams friend updates', async () => {
    const sockets: FakeWebSocket[] = [];
    const client = new PresenceClient({
      url: 'ws://example.test/api/v1/presence',
      identity: fakeIdentity(),
      webSocketFactory: () => {
        const socket = new FakeWebSocket();
        sockets.push(socket);
        return socket as unknown as WebSocket;
      },
    });
    client.setContacts(['clr-JOYG-7DSO']);
    client.announce({
      room: {
        roomId: 'room',
        viewerUrl: 'https://example.test/r/room?access=public#secret',
        viewerCount: 0,
        sharingState: 'idle',
      },
      presenterSecret: 'presenter-secret',
    });
    client.connect();

    const socket = sockets[0]!;
    socket.open();
    expect(client.getSnapshot().status).toBe('authenticating');

    socket.receive(challenge('nonce-1'));
    await flush();
    const hello = JSON.parse(socket.sent[0]!) as { type: string; publicKey: string; signature: string };
    expect(hello.type).toBe('presence:hello');
    expect(hello.publicKey).toBe('cHVibGlj');
    expect(hello.signature).not.toHaveLength(0);

    socket.receive(ready('clr-SELF-CODE'));
    expect(client.getSnapshot().status).toBe('ready');
    expect(client.getSnapshot().selfCode).toBe('clr-SELF-CODE');
    const replayed = socket.sent.slice(1).map((raw) => (JSON.parse(raw) as { type: string }).type);
    expect(replayed).toEqual(['presence:subscribe', 'presence:announce']);

    socket.receive(snapshot([friend('clr-JOYG-7DSO', true)]));
    expect(client.getSnapshot().friends).toHaveLength(1);

    socket.receive(update(friend('clr-JOYG-7DSO', false, 120)));
    expect(client.getSnapshot().friends).toEqual([friend('clr-JOYG-7DSO', false, 120)]);

    // Incoming friend requests arrive as a replace-set message.
    socket.receive(requests(['clr-RQGM-C6QE']));
    expect(client.getSnapshot().requests).toEqual(['clr-RQGM-C6QE']);
    socket.receive(requests([]));
    expect(client.getSnapshot().requests).toEqual([]);
  });

  it('reconnects with backoff and replays subscription and announcement', async () => {
    vi.useFakeTimers();
    try {
      const sockets: FakeWebSocket[] = [];
      const client = new PresenceClient({
        url: 'ws://example.test/api/v1/presence',
        identity: fakeIdentity(),
        webSocketFactory: () => {
          const socket = new FakeWebSocket();
          sockets.push(socket);
          return socket as unknown as WebSocket;
        },
      });
      client.setContacts(['clr-JOYG-7DSO']);
      client.connect();
      sockets[0]!.open();
      sockets[0]!.finishClose();
      expect(client.getSnapshot().status).toBe('reconnecting');

      await vi.advanceTimersByTimeAsync(20_000);
      const socket = sockets[1]!;
      expect(socket).toBeDefined();
      socket.open();
      socket.receive(challenge('nonce-2'));
      await vi.advanceTimersByTimeAsync(0);
      socket.receive(ready('clr-SELF-CODE'));
      const types = socket.sent.map((raw) => (JSON.parse(raw) as { type: string }).type);
      expect(types).toEqual(['presence:hello', 'presence:subscribe']);
      expect(client.getSnapshot().status).toBe('ready');
    } finally {
      vi.useRealTimers();
    }
  });

  it('stops retrying when the identity is rejected', () => {
    const sockets: FakeWebSocket[] = [];
    const client = new PresenceClient({
      url: 'ws://example.test/api/v1/presence',
      identity: fakeIdentity(),
      webSocketFactory: () => {
        const socket = new FakeWebSocket();
        sockets.push(socket);
        return socket as unknown as WebSocket;
      },
    });
    client.connect();
    sockets[0]!.open();
    sockets[0]!.receive({
      type: 'error',
      protocolVersion: PROTOCOL_VERSION,
      serverTimestamp: 'now',
      code: 'authentication_failed',
      message: 'The presence identity could not be verified.',
    });
    expect(client.getSnapshot().status).toBe('failed');
    sockets[0]!.finishClose();
    expect(sockets).toHaveLength(1);
  });
});

function fakeIdentity(): PresenceIdentity {
  return {
    publicKeyBase64: 'cHVibGlj',
    sign: (message) => Promise.resolve(new Uint8Array(64).fill(message.length % 255)),
  };
}

function challenge(nonce: string): PresenceServerMessage {
  return {
    type: 'presence:challenge',
    protocolVersion: PROTOCOL_VERSION,
    serverTimestamp: 'now',
    nonce,
  };
}

function ready(code: string): PresenceServerMessage {
  return {
    type: 'presence:ready',
    protocolVersion: PROTOCOL_VERSION,
    serverTimestamp: 'now',
    code,
  };
}

function snapshot(friends: FriendPresence[]): PresenceServerMessage {
  return {
    type: 'presence:snapshot',
    protocolVersion: PROTOCOL_VERSION,
    serverTimestamp: 'now',
    friends,
  };
}

function update(friend: FriendPresence): PresenceServerMessage {
  return {
    type: 'presence:update',
    protocolVersion: PROTOCOL_VERSION,
    serverTimestamp: 'now',
    friend,
  };
}

function requests(codes: string[]): PresenceServerMessage {
  return {
    type: 'presence:requests',
    protocolVersion: PROTOCOL_VERSION,
    serverTimestamp: 'now',
    codes,
  };
}

function friend(
  code: string,
  online: boolean,
  lastSeenSecondsAgo: number | null = null,
): FriendPresence {
  return { code, online, hosting: null, lastSeenSecondsAgo };
}

async function flush(): Promise<void> {
  for (let i = 0; i < 5; i += 1) await Promise.resolve();
}

class FakeWebSocket extends EventTarget {
  public readyState: number = WebSocket.CONNECTING;
  public readonly sent: string[] = [];

  public open(): void {
    this.readyState = WebSocket.OPEN;
    this.dispatchEvent(new Event('open'));
  }

  public receive(message: PresenceServerMessage): void {
    this.dispatchEvent(new MessageEvent('message', { data: JSON.stringify(message) }));
  }

  public send(value: string): void {
    this.sent.push(value);
  }

  public close(): void {
    this.readyState = WebSocket.CLOSING;
  }

  public finishClose(): void {
    this.readyState = WebSocket.CLOSED;
    this.dispatchEvent(new CloseEvent('close'));
  }
}
