// Friend codes: a short, human-tradeable fingerprint of an identity's public
// key, formatted `clr-XXXX-XXXX`. Mirrors `clarity_protocol::code`: the first
// 40 bits of SHA-256 over the 32-byte Ed25519 public key, RFC 4648 base32.

const PREFIX = 'clr';
const BODY_LENGTH = 8;
const BASE32_ALPHABET = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567';

export async function friendCodeForPublicKey(
  publicKey: Uint8Array,
  subtle: SubtleCrypto = crypto.subtle,
): Promise<string> {
  const digest = new Uint8Array(
    await subtle.digest('SHA-256', publicKey.slice().buffer),
  );
  const body = base32Encode(digest.subarray(0, 5));
  return `${PREFIX}-${body.slice(0, 4)}-${body.slice(4, 8)}`;
}

/**
 * Parses a user-entered code into canonical form, tolerating case, spaces, a
 * missing `clr` prefix, and missing or extra dashes. Returns `null` if the
 * body is not exactly eight base32 characters.
 */
export function normalizeFriendCode(input: string): string | null {
  const cleaned = input.replace(/[^a-z0-9]/gi, '').toUpperCase();
  const body = cleaned.startsWith(PREFIX.toUpperCase())
    ? cleaned.slice(PREFIX.length)
    : cleaned;
  if (body.length !== BODY_LENGTH || !/^[A-Z2-7]+$/.test(body)) {
    return null;
  }
  return `${PREFIX}-${body.slice(0, 4)}-${body.slice(4, 8)}`;
}

export function isValidFriendCode(input: string): boolean {
  return normalizeFriendCode(input) !== null;
}

function base32Encode(bytes: Uint8Array): string {
  let bits = 0;
  let value = 0;
  let output = '';
  for (const byte of bytes) {
    value = (value << 8) | byte;
    bits += 8;
    while (bits >= 5) {
      output += BASE32_ALPHABET[(value >>> (bits - 5)) & 31];
      bits -= 5;
    }
  }
  if (bits > 0) {
    output += BASE32_ALPHABET[(value << (5 - bits)) & 31];
  }
  return output;
}
