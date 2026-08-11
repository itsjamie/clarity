/**
 * Domain separation for identity-challenge signatures, mirroring
 * `identity_challenge_payload` in `crates/clarity-protocol/src/lib.rs`.
 *
 * Binding a context tag and the server's `host[:port]` into the signed bytes
 * gives every signature a single purpose on a single server: a hostile server
 * relaying another server's nonce (or replaying a presence signature into
 * room authentication) obtains a signature over the wrong payload, which
 * never verifies.
 */
export type IdentityChallengeContext = 'room-auth' | 'presence';

/**
 * The exact string an identity signs to answer a challenge from the server at
 * `serverUrl`. `URL#host` omits default ports, matching the server's own
 * canonicalization of its public base URL and allowed origins.
 */
export function identityChallengePayload(
  context: IdentityChallengeContext,
  serverUrl: string,
  nonce: string,
): string {
  return `clarity-identity:v1:${context}:${new URL(serverUrl).host}:${nonce}`;
}
