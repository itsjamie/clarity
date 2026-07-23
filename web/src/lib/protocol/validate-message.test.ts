import { parseServerMessage, ProtocolValidationError } from './validate-message';

describe('server message validation', () => {
  it('accepts a valid heartbeat', () => {
    expect(
      parseServerMessage(
        JSON.stringify({
          type: 'heartbeat:ping',
          protocolVersion: 2,
          serverTimestamp: '2026-01-01T00:00:00Z',
          nonce: 'nonce',
        }),
      ).type,
    ).toBe('heartbeat:ping');
  });

  it('rejects malformed and unsupported shapes', () => {
    expect(() => parseServerMessage('{')).toThrow(ProtocolValidationError);
    expect(() => parseServerMessage('{"type":"signal:offer"}')).toThrow(
      ProtocolValidationError,
    );
  });
});
