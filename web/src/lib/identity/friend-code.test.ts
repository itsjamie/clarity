import { webcrypto } from 'node:crypto';

import {
  friendCodeForPublicKey,
  isValidFriendCode,
  normalizeFriendCode,
} from './friend-code';

const subtle = webcrypto.subtle as SubtleCrypto;

describe('friend codes', () => {
  it('derives the same code as clarity_protocol::code::encode', async () => {
    // Vectors cross-checked against SHA-256 + RFC 4648 base32 of the key.
    await expect(friendCodeForPublicKey(new Uint8Array(32).fill(7), subtle)).resolves.toBe(
      'clr-JOYG-7DSO',
    );
    await expect(friendCodeForPublicKey(new Uint8Array(32).fill(9), subtle)).resolves.toBe(
      'clr-RQGM-C6QE',
    );
  });

  it('produces well-formed, normalizable codes', async () => {
    const code = await friendCodeForPublicKey(new Uint8Array(32).fill(1), subtle);
    expect(code).toMatch(/^clr-[A-Z2-7]{4}-[A-Z2-7]{4}$/);
    expect(normalizeFriendCode(code)).toBe(code);
    expect(isValidFriendCode(code)).toBe(true);
  });

  it('normalizes messy input like the Rust parser', () => {
    expect(normalizeFriendCode('  joyg 7dso  ')).toBe('clr-JOYG-7DSO');
    expect(normalizeFriendCode('CLR-joyg-7DSO')).toBe('clr-JOYG-7DSO');
    expect(normalizeFriendCode('clrJOYG7DSO')).toBe('clr-JOYG-7DSO');
    expect(normalizeFriendCode('joyg7dso')).toBe('clr-JOYG-7DSO');
  });

  it('rejects the wrong length and alphabet', () => {
    expect(normalizeFriendCode('clr-abc')).toBeNull();
    expect(normalizeFriendCode('clr-0000-1111')).toBeNull(); // 0 and 1 are not base32
    expect(normalizeFriendCode('')).toBeNull();
    expect(isValidFriendCode('clr-JOYG-7DSO-XX')).toBe(false);
  });
});
