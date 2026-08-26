import {
  CHAT_CHANNEL_LABEL,
  CHAT_MAX_BUFFERED_BYTES,
  CHAT_MAX_PAYLOAD_BYTES,
  CHAT_MAX_TEXT_CHARACTERS,
  chatPayloadBytes,
  decodeChatMessage,
  encodeChatMessage,
  relayChatPayload,
  trySendChatPayload,
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

  it('rejects oversized decoded and encoded messages', () => {
    const oversizedText = 'x'.repeat(CHAT_MAX_TEXT_CHARACTERS + 1);
    expect(() => encodeChatMessage({ sender: 'June', text: oversizedText })).toThrow(RangeError);
    expect(decodeChatMessage(JSON.stringify({ sender: 'June', text: oversizedText }))).toBeNull();
    expect(decodeChatMessage('x'.repeat(CHAT_MAX_PAYLOAD_BYTES + 1))).toBeNull();
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

  it('drops delivery to backpressured or failed channels without aborting the fanout', () => {
    const backpressured = new FakeChannel('open', CHAT_MAX_BUFFERED_BYTES + 1);
    const failed = new FakeChannel('open', 0, true);
    const healthy = new FakeChannel('open');

    const delivered = relayChatPayload(
      [
        ['viewer-1', backpressured],
        ['viewer-2', failed],
        ['viewer-3', healthy],
      ],
      'presenter',
      'payload',
    );

    expect(delivered).toEqual(['viewer-3']);
    expect(backpressured.sent).toEqual([]);
    expect(healthy.sent).toEqual(['payload']);
  });

  it('never lets one send push the buffered amount over the cap', () => {
    const payload = encodeChatMessage({ sender: 'June', text: 'hello' });
    const payloadBytes = chatPayloadBytes(payload);
    const fitsExactly = new FakeChannel('open', CHAT_MAX_BUFFERED_BYTES - payloadBytes);
    const exceedsByOne = new FakeChannel(
      'open',
      CHAT_MAX_BUFFERED_BYTES - payloadBytes + 1,
    );

    expect(trySendChatPayload(fitsExactly, payload)).toBe(true);
    expect(trySendChatPayload(exceedsByOne, payload)).toBe(false);
    expect(fitsExactly.sent).toEqual([payload]);
    expect(exceedsByOne.sent).toEqual([]);
  });
});

class FakeChannel implements ChatChannelLike {
  readonly sent: string[] = [];

  public constructor(
    public readonly readyState: RTCDataChannelState,
    public readonly bufferedAmount = 0,
    private readonly fail = false,
  ) {}

  public send(payload: string): void {
    if (this.fail) throw new Error('send failed');
    this.sent.push(payload);
  }
}
