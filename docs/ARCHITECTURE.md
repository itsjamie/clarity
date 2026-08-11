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

### `clarity-media`

Media engine for native clients. `Playback` receives one presenter's WebRTC stream and renders it into a dedicated window: presenter offers and ICE candidates go in as protocol domain values, and the SDP answer, local candidates, connection states, and receive statistics come out on an event channel. Remote candidates are accepted in any order relative to the offer, repeated offers renegotiate the same connection, and missing audio is reported rather than failed. Codec preferences follow the presenter's offer order filtered to what the machine can decode, and the answer preserves the transport-wide congestion control extension the presenter's bandwidth ramp depends on.

`Broadcast` sends one video source to up to the room's viewer capacity, each viewer on an independent connection with its own encoder, mirroring the per-viewer quality model of the web presenter. Video is encoded with hardware H.264 (NVENC) when available — a steady CBR bitrate at high resolution without CPU cost — and falls back to software VP8 otherwise; the source is normalized once to the chosen encoder's pixel format ahead of the tee, so per-viewer branches only encode. The broadcast is the offerer; viewer ids are caller-chosen, operations on unknown viewers are ignored, per-viewer bitrate is adjustable live, and pausing stops media flow without renegotiation. Sources are a synthetic test pattern or a screen capture: `CaptureStream` negotiates the compositor's ScreenCast portal — system picker, cursor embedding, and opt-in restore tokens for silent reuse — and hands `Broadcast` a live PipeWire stream that is revoked when the broadcast ends. By default no grant is retained anywhere and the picker appears on every run.

Each adaptive viewer negotiates transport-wide congestion control and is steered by its own controller in the style of Google Congestion Control: delay overuse or heavy loss multiplicatively decreases the rate anchored to the observed receive rate, mild loss holds, and a clean window ramps toward the configured ceiling. Fixed viewers hold their ceiling regardless of feedback.

Audio rides each viewer's connection as a second Opus stream with in-band FEC: the system mix (the default output's monitor) by default, or a mix of specific PipeWire playback streams — one application on its own, or the system's audio with named applications (a voice call, say) filtered out. The picked window cannot select its own audio automatically, because the ScreenCast portal does not disclose which application owns a window and has no audio channel at all; per-application audio is therefore resolved client-side against the applications currently playing, snapshotted when sharing starts. An audio source that cannot be captured downgrades the broadcast to video-only rather than failing, and pause stops both streams without renegotiation.

GStreamer is the hidden implementation of both engines and does not cross the crate boundary. `CLARITY_MEDIA_HEADLESS` consumes media without a window or audio device for tests and displayless environments.

### `clarity-client`

The native client binary (`clarity`). A signaling client owns the WebSocket lifecycle — authentication, session resume, heartbeat replies, and the same bounded reconnect backoff as the web client — and sends the exact `Origin` the server allowlists, derived from the invitation or server URL. `ViewerSession` mirrors the web viewer session's state machine and drives one `Playback`; `PresenterSession` mirrors the web presenter's admission model, treating the server's room snapshot as the authority on which viewer connections exist, and drives one `Broadcast`. `clarity view <invitation-url>` joins a room as a viewer; `clarity present` creates a room over HTTP, prints the viewer invitation, and streams until the room ends. Secrets are parsed once and never logged.

Peer recovery mirrors the web presenter. A viewer connection that fails triggers an ICE restart; if it has not recovered within an eight-second grace window, the presenter tears that connection down and rebuilds it from a fresh encoder and transport, and the viewer recreates its `Playback` when the resulting plain offer arrives over an existing connection. A viewer that detects its own connection failing asks the presenter to restart, covering asymmetric breaks. A signaling reconnect never disturbs media: re-authentication with the resume token keeps the broadcast and every viewer connection running, refreshing only ICE configuration in case TURN credentials rotated.

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

The presenter calls `getDisplayMedia` only from a Start Sharing or Change Source user gesture. Text and Motion modes select distinct content hints and requested constraints. Capture resolution defaults to 2560 x 1440 and can be raised to 3840 x 2160; the target is applied directly to the browser-native capture track without an application canvas or extra scaling pass. Shared audio is requested by default but can be disabled before capture; window selection prefers application audio when the browser supports it. Microphone capture is never requested. A single captured video track, plus shared audio if the browser provides it, is attached to one independent `RTCPeerConnection` per approved viewer.

Invitation authentication always precedes admission. Public rooms immediately admit invitation holders while capacity remains. Approval-required rooms keep authenticated viewers pending and prohibit SDP exchange until the presenter approves them.

Each presenter peer has its own sender, encoding profile, stats collector, adaptation controller, ICE recovery counter, and lifecycle. Adding, removing, degrading, or restarting ICE for one viewer does not intentionally touch any other peer. Change Source uses `RTCRtpSender.replaceTrack`, preserving negotiated peer connections where the browser allows it.

## Quality model

Requested encoding limits and measured receiver/sender statistics are reported separately. Bitrate is derived from byte-counter deltas. The UI reports available resolution, FPS, codec, loss, RTT, limitation reason, and direct/relay path.

Adaptive mode uses smoothed samples, three unhealthy samples before degradation, fifteen healthy samples before upgrade, separate cooldowns, and bitrate headroom. Text profiles preserve resolution longer; Motion profiles preserve frame rate longer. Fixed mode bypasses automatic profile changes. Total estimated presenter upload is the selected per-viewer ceiling multiplied by active viewers; actual total is the sum of peer stats.

## Session recovery

WebSocket loss schedules bounded reconnect attempts using the opaque resume token stored in `sessionStorage`. Healthy media remains open during signaling recovery. Presenter and viewer records have configurable grace windows. ICE failure triggers an isolated restart for that peer. Room closure, expiry, capture-ended events, and graceful server shutdown cancel timers, tracks, sockets, actors, and peer connections.
