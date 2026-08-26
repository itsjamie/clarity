import type { ChatMessage } from '@/generated/protocol';

/**
 * The reliable data channel label shared with the native engine
 * (`crates/clarity-media/src/broadcast.rs`, `create_chat_channel`). Both
 * sides must use this exact label and the `ChatMessage` JSON envelope so
 * web and native peers interoperate in the same room.
 */
export const CHAT_CHANNEL_LABEL = 'chat';
export const CHAT_MAX_SENDER_CHARACTERS = 48;
export const CHAT_MAX_TEXT_CHARACTERS = 2_000;
export const CHAT_MAX_PAYLOAD_BYTES = 8 * 1_024;
export const CHAT_MAX_QUEUED_MESSAGES = 32;
export const CHAT_MAX_BUFFERED_BYTES = 64 * 1_024;

const encoder = new TextEncoder();

export function encodeChatMessage(message: ChatMessage): string {
  if (
    !hasAtMostCharacters(message.sender, CHAT_MAX_SENDER_CHARACTERS) ||
    !hasAtMostCharacters(message.text, CHAT_MAX_TEXT_CHARACTERS)
  ) {
    throw new RangeError('Chat message exceeds the supported character limit.');
  }
  const payload = JSON.stringify({ sender: message.sender, text: message.text });
  if (!isBoundedPayload(payload)) {
    throw new RangeError('Chat message exceeds the supported payload limit.');
  }
  return payload;
}

/** Parses a data channel payload; non-envelope payloads yield `null`. */
export function decodeChatMessage(payload: unknown): ChatMessage | null {
  if (typeof payload !== 'string') return null;
  if (!isBoundedPayload(payload)) return null;
  let value: unknown;
  try {
    value = JSON.parse(payload);
  } catch {
    return null;
  }
  if (typeof value !== 'object' || value === null) return null;
  const candidate = value as Record<string, unknown>;
  if (typeof candidate.sender !== 'string' || typeof candidate.text !== 'string') return null;
  if (
    !hasAtMostCharacters(candidate.sender, CHAT_MAX_SENDER_CHARACTERS) ||
    !hasAtMostCharacters(candidate.text, CHAT_MAX_TEXT_CHARACTERS)
  ) {
    return null;
  }
  return { sender: candidate.sender, text: candidate.text };
}

export interface ChatChannelLike {
  readonly readyState: RTCDataChannelState;
  readonly bufferedAmount: number;
  send(payload: string): void;
}

/** Sends one bounded payload without allowing a slow peer's SCTP queue to grow indefinitely. */
export function trySendChatPayload(channel: ChatChannelLike | null, payload: string): boolean {
  const payloadBytes = chatPayloadBytes(payload);
  if (
    channel?.readyState !== 'open' ||
    !isBoundedPayload(payload, payloadBytes) ||
    channel.bufferedAmount + payloadBytes > CHAT_MAX_BUFFERED_BYTES
  ) {
    return false;
  }
  try {
    channel.send(payload);
    return true;
  } catch {
    return false;
  }
}

export function chatPayloadBytes(payload: string): number {
  return encoder.encode(payload).byteLength;
}

function isBoundedPayload(payload: string, payloadBytes = chatPayloadBytes(payload)): boolean {
  return payload.length <= CHAT_MAX_PAYLOAD_BYTES && payloadBytes <= CHAT_MAX_PAYLOAD_BYTES;
}

function hasAtMostCharacters(value: string, maximum: number): boolean {
  let count = 0;
  const characters = value[Symbol.iterator]();
  while (!characters.next().done) {
    count += 1;
    if (count > maximum) return false;
  }
  return true;
}

/**
 * Presenter-side relay: forwards an already-encoded envelope to every other
 * open channel, so one presenter acts as the chat hub without the server. The
 * caller decodes the incoming payload and stamps its `sender` before encoding;
 * this function only fans the result out. Returns the peer ids the payload was
 * delivered to.
 */
export function relayChatPayload(
  channels: Iterable<readonly [string, ChatChannelLike | null]>,
  fromPeerId: string,
  payload: string,
): string[] {
  const delivered: string[] = [];
  for (const [peerId, channel] of channels) {
    if (peerId === fromPeerId) continue;
    if (trySendChatPayload(channel, payload)) delivered.push(peerId);
  }
  return delivered;
}
