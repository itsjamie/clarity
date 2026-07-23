# Security model

## Credentials and invitations

- Public room IDs contain 96 bits of OS randomness and are identifiers, not credentials.
- Presenter, viewer-invitation, and resume secrets contain 256 bits of OS randomness.
- The presenter secret is returned once, stored in `sessionStorage`, and never placed in a URL.
- Viewer invitations use `/r/{roomId}#{secret}`. The browser reads the fragment, stores it in `sessionStorage`, and immediately removes it with `history.replaceState`.
- The Rust service retains domain-separated HMAC-SHA-256 digests, not raw room secrets, and compares supplied digests in constant time.
- Raw secrets use `secrecy::SecretString`; Debug output and structured application logs redact them.

Anyone possessing an invitation can enter a public room while capacity remains. In an approval-required room, the presenter sees a self-supplied label and must approve the browser. Neither policy provides verified viewer identity.

## Signaling authorization

Every socket must authenticate within the configured deadline. The server assigns peer IDs and rejects pre-authentication commands, unsupported versions, role-inappropriate commands, cross-room destinations, viewer-to-viewer signaling, and SDP exchange by pending viewers. Public-room viewers become approved atomically during authentication; approval-required viewers cannot exchange SDP until the presenter admits them. Message/frame size limits, bounded queues, heartbeats, origin checks, and per-source/per-session rate limits constrain resource use.

Logs include request/room/peer context where useful but omit raw SDP, ICE candidates, credentials, invitation fragments, and TURN secrets. Client diagnostic exports redact credential-like keys, URLs, candidates, SDP, and IP-shaped strings.

## Browser and HTTP policy

The application emits CSP, HSTS in production, `Referrer-Policy: no-referrer`, `X-Content-Type-Options: nosniff`, and a Permissions Policy that denies camera and microphone while allowing self display capture. API secret responses are `Cache-Control: no-store`; hashed frontend assets are immutable. Unknown API paths return JSON rather than SPA HTML.

Production should keep frontend and API on one HTTPS origin. `ALLOWED_ORIGINS` is an explicit comma-separated allowlist; do not use wildcard origins. Credentials are sent only inside authenticated WSS messages, never in a WebSocket URL or query string.

## TURN credentials

The long-lived `TURN_SHARED_SECRET` exists only in the Rust and coturn environments. Authenticated sessions receive short-lived coturn REST credentials derived with HMAC-SHA-1 solely for protocol compatibility. Rotate the shared secret during a maintenance window; existing credentials signed with the old value stop working when coturn switches. Room/resume HMAC key rotation invalidates active credentials and therefore also requires a restart window.

Generate three independent keys, for example:

```bash
openssl rand -base64 48
openssl rand -base64 48
openssl rand -base64 48
```

Do not commit `.env`, include it in support bundles, or place secrets in process arguments. Restrict it to the deployment account (`chmod 600 .env`).

## Threat and privacy limits

- Direct WebRTC exposes ICE network endpoints to the connected peer. TURN relay reduces direct peer-IP exposure but does not make viewers anonymous to the service operator.
- DTLS-SRTP protects media in transit, including through TURN; Clarity Share does not add application-layer end-to-end identity verification.
- A malicious modified client cannot be forced to obey display-only UI policy and can use external recording tools.
- The single VPS is a single point of failure. In-memory rooms are lost on restart.
- Rate limiting is process-local and intentionally does not replace upstream volumetric DDoS protection.
- Keep Rust, the base image, Caddy, coturn, and host packages patched. Review image digests and release notes before upgrades.

Report suspected vulnerabilities privately to the deployment operator. Avoid attaching invitations, `.env`, raw WebRTC internals, or unsanitized browser logs.
