import type { ChatMessage } from '@/generated/protocol';

/**
 * The reliable data channel label shared with the native engine
 * (`crates/clarity-media/src/broadcast.rs`, `create_chat_channel`). Both
 * sides must use this exact label and the `ChatMessage` JSON envelope so
 * web and native peers interoperate in the same room.
 */
export const CHAT_CHANNEL_LABEL = 'chat';

export function encodeChatMessage(message: ChatMessage): string {
  return JSON.stringify({ sender: message.sender, text: message.text });
}

/** Parses a data channel payload; non-envelope payloads yield `null`. */
export function decodeChatMessage(payload: unknown): ChatMessage | null {
  if (typeof payload !== 'string') return null;
  let value: unknown;
  try {
    value = JSON.parse(payload);
  } catch {
    return null;
  }
  if (typeof value !== 'object' || value === null) return null;
  const candidate = value as Record<string, unknown>;
  if (typeof candidate.sender !== 'string' || typeof candidate.text !== 'string') return null;
  return { sender: candidate.sender, text: candidate.text };
}

export interface ChatChannelLike {
  readonly readyState: RTCDataChannelState;
  send(payload: string): void;
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
    if (peerId === fromPeerId || channel?.readyState !== 'open') continue;
    channel.send(payload);
    delivered.push(peerId);
  }
  return delivered;
}
