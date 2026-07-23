import { reconnectDelay, SignalingClient } from './signaling-client';

describe('signaling reconnect backoff', () => {
  it('uses bounded exponential delays with jitter', () => {
    expect(reconnectDelay(0, 0)).toBe(375);
    expect(reconnectDelay(1, 0.5)).toBe(1000);
    expect(reconnectDelay(20, 1)).toBe(12_500);
  });

  it('ignores a stale socket close after a replacement connection starts', () => {
    vi.useFakeTimers();
    const sockets: FakeWebSocket[] = [];
    const client = new SignalingClient({
      url: 'ws://example.test/api/v1/ws',
      roomId: 'room',
      role: 'presenter',
      authentication: {
        type: 'auth:presenter',
        protocolVersion: 2,
        requestId: 'auth',
        roomId: 'room',
        presenterSecret: 'secret',
      },
      onMessage: vi.fn(),
      onStateChange: vi.fn(),
      storage: memoryStorage(),
      webSocketFactory: () => {
        const socket = new FakeWebSocket();
        sockets.push(socket);
        return socket as unknown as WebSocket;
      },
    });

    client.connect();
    const stale = sockets[0]!;
    client.disconnect(false);
    client.connect();
    sockets[1]!.open();
    stale.finishClose();
    vi.advanceTimersByTime(20_000);

    expect(sockets).toHaveLength(2);
    vi.useRealTimers();
  });
});

class FakeWebSocket extends EventTarget {
  public readyState: number = WebSocket.CONNECTING;
  public readonly sent: string[] = [];

  public open(): void {
    this.readyState = WebSocket.OPEN;
    this.dispatchEvent(new Event('open'));
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

function memoryStorage(): Pick<Storage, 'getItem' | 'setItem' | 'removeItem'> {
  const values = new Map<string, string>();
  return {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
    removeItem: (key) => values.delete(key),
  };
}
