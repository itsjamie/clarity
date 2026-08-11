import {
  CHAT_CHANNEL_LABEL,
  decodeChatMessage,
  encodeChatMessage,
  relayChatPayload,
  type ChatChannelLike,
} from './chat-channel';

describe('chat envelope', () => {
  it('uses the label shared with the native engine', () => {
    expect(CHAT_CHANNEL_LABEL).toBe('chat');
  });

  it('round-trips the ChatMessage envelope', () => {
    const payload = encodeChatMessage({ sender: 'June', text: 'watch the left column' });
    expect(JSON.parse(payload)).toEqual({ sender: 'June', text: 'watch the left column' });
    expect(decodeChatMessage(payload)).toEqual({ sender: 'June', text: 'watch the left column' });
  });

  it('drops payloads that are not the envelope', () => {
    expect(decodeChatMessage('plain text')).toBeNull();
    expect(decodeChatMessage('42')).toBeNull();
    expect(decodeChatMessage('null')).toBeNull();
    expect(decodeChatMessage(JSON.stringify({ sender: 'x' }))).toBeNull();
    expect(decodeChatMessage(JSON.stringify({ sender: 1, text: 'y' }))).toBeNull();
    expect(decodeChatMessage(new ArrayBuffer(4))).toBeNull();
  });

  it('keeps unknown extra fields out of the decoded message', () => {
    const decoded = decodeChatMessage(JSON.stringify({ sender: 'a', text: 'b', extra: true }));
    expect(decoded).toEqual({ sender: 'a', text: 'b' });
  });
});

describe('presenter chat relay', () => {
  it('forwards the raw payload to every other open channel', () => {
    const channels = new Map<string, FakeChannel>([
      ['viewer-1', new FakeChannel('open')],
      ['viewer-2', new FakeChannel('open')],
      ['viewer-3', new FakeChannel('open')],
    ]);
    const payload = encodeChatMessage({ sender: 'June', text: 'hello' });

    const delivered = relayChatPayload(channels, 'viewer-1', payload);

    expect(delivered).toEqual(['viewer-2', 'viewer-3']);
    expect(channels.get('viewer-1')?.sent).toEqual([]);
    expect(channels.get('viewer-2')?.sent).toEqual([payload]);
    expect(channels.get('viewer-3')?.sent).toEqual([payload]);
  });

  it('skips channels that are not open or missing', () => {
    const open = new FakeChannel('open');
    const connecting = new FakeChannel('connecting');
    const channels: Array<readonly [string, ChatChannelLike | null]> = [
      ['viewer-1', open],
      ['viewer-2', connecting],
      ['viewer-3', null],
    ];

    const delivered = relayChatPayload(channels, 'viewer-4', 'payload');

    expect(delivered).toEqual(['viewer-1']);
    expect(open.sent).toEqual(['payload']);
    expect(connecting.sent).toEqual([]);
  });
});

class FakeChannel implements ChatChannelLike {
  readonly sent: string[] = [];

  public constructor(public readonly readyState: RTCDataChannelState) {}

  public send(payload: string): void {
    this.sent.push(payload);
  }
}
