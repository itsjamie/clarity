import type { ServerMessage } from '@/generated/protocol';
import validate from '@/generated/server-message-validator.js';

export class ProtocolValidationError extends Error {
  public constructor() {
    super('The server returned an invalid signaling message.');
    this.name = 'ProtocolValidationError';
  }
}

export function parseServerMessage(input: string): ServerMessage {
  let value: unknown;
  try {
    value = JSON.parse(input) as unknown;
  } catch {
    throw new ProtocolValidationError();
  }
  if (!validate(value)) {
    throw new ProtocolValidationError();
  }
  return value as ServerMessage;
}
