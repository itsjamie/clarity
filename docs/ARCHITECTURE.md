# Architecture

## Runtime topology

Clarity Share runs on one public Linux VPS:

```text
Browser -- HTTPS/WSS --> Caddy --> clarity-server
Browser -- STUN/TURN ----------------> coturn
Presenter == encrypted WebRTC media ==> Viewer 1..10
```

Caddy terminates application HTTPS and proxies HTTP and WebSocket traffic. `clarity-server` serves the embedded React build and owns all application policy. Coturn provides STUN and TURN only. Media flows directly between browsers when ICE permits, or as encrypted packets through coturn. It never passes through Axum.

## Rust workspace

### `clarity-protocol`

The protocol crate is the source of truth for HTTP payloads, access policies, roles, room/viewer states, ICE configuration, errors, and every client/server WebSocket variant. Serde-tagged unions use `type`, camel-case fields, protocol version 2, request IDs for request/response correlation, and server timestamps on live events. Schemars and ts-rs feed deterministic artifacts in `web/src/generated`.

### `clarity-core`

- `crypto`: URL-safe OS-random room IDs and secrets, domain-separated HMAC-SHA-256 digests, constant-time verification, and redacted secret wrappers.
- `room`: the registry, one bounded-command actor per room, public or approval-required admission, capacity rules, resume records, signal authorization, expiration, and deterministic cleanup.
- `turn`: short-lived coturn REST usernames and HMAC-SHA-1 credentials.
- `clock`: system and injectable test clocks.

The global registry holds only room IDs and actor handles. Exactly one Tokio task owns each room's mutable state; network writers receive messages through bounded queues. No room lock is held across an await.

### `clarity-server`

- `config`: development defaults plus strict production validation.
- `app`: health/readiness, room creation, exact-origin validation, response headers, API errors, and embedded SPA routing.
- `ws`: upgrade authentication, frame limits, one reader and writer per socket, heartbeats, session limits, redacted logging, and room-command routing.
- `rate_limit`: bounded per-source and per-session token buckets.
- `main`: structured logging, listener startup, signals, registry shutdown, and graceful drain.

## Frontend dependency direction

The frontend follows Bulletproof React's feature-oriented structure:

```text
src/main.tsx
  -> app/                 composition, providers, routes
     -> features/         room-creation, presenter, viewer
        -> shared modules components/, config/, hooks/, lib/, utils/
```

Shared modules do not import application or feature code. Feature modules do not import one another. ESLint enforces those zones, cycles, strict type-aware rules, and the ban on `any`.

React renders state and invokes focused session objects. `PresenterSession` and `ViewerSession` own reconnection/resumption and coordinate dedicated WebRTC services outside the component lifecycle.

## Media lifecycle

The presenter calls `getDisplayMedia` only from a Start Sharing or Change Source user gesture. Text and Motion modes select distinct content hints and requested constraints. Shared audio is requested by default but can be disabled before capture; window selection prefers application audio when the browser supports it. Microphone capture is never requested. A single captured video track, plus shared audio if the browser provides it, is attached to one independent `RTCPeerConnection` per approved viewer.

Invitation authentication always precedes admission. Public rooms immediately admit invitation holders while capacity remains. Approval-required rooms keep authenticated viewers pending and prohibit SDP exchange until the presenter approves them.

Each presenter peer has its own sender, encoding profile, stats collector, adaptation controller, ICE recovery counter, and lifecycle. Adding, removing, degrading, or restarting ICE for one viewer does not intentionally touch any other peer. Change Source uses `RTCRtpSender.replaceTrack`, preserving negotiated peer connections where the browser allows it.

## Quality model

Requested encoding limits and measured receiver/sender statistics are reported separately. Bitrate is derived from byte-counter deltas. The UI reports available resolution, FPS, codec, loss, RTT, limitation reason, and direct/relay path.

Adaptive mode uses smoothed samples, three unhealthy samples before degradation, fifteen healthy samples before upgrade, separate cooldowns, and bitrate headroom. Text profiles preserve resolution longer; Motion profiles preserve frame rate longer. Fixed mode bypasses automatic profile changes. Total estimated presenter upload is the selected per-viewer ceiling multiplied by active viewers; actual total is the sum of peer stats.

## Session recovery

WebSocket loss schedules bounded reconnect attempts using the opaque resume token stored in `sessionStorage`. Healthy media remains open during signaling recovery. Presenter and viewer records have configurable grace windows. ICE failure triggers an isolated restart for that peer. Room closure, expiry, capture-ended events, and graceful server shutdown cancel timers, tracks, sockets, actors, and peer connections.
