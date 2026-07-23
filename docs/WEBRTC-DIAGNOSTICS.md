# WebRTC diagnostics

## What the UI reports

Presenter peer cards show connection/ICE state, direct or TURN relay path, measured bitrate, encoded resolution, FPS, RTT, packet loss, codec, requested sender profile, and the latest adaptation reason. The P2P upload panel plots the sum of those measured sender bitrates over a trailing 30-second window and keeps that measured total separate from the estimated upload for the requested profile.

Viewer HUD values come from inbound RTP and the selected ICE candidate pair. `Unavailable` means the browser omitted that statistic; it is not converted into zero. Bitrate is calculated from byte-counter deltas rather than the cumulative counter.

The Advanced diagnostics export is deliberately sanitized. It removes secret/token/credential/SDP/candidate/address/IP/URL-shaped fields and bounds the event history. It is useful for state timelines, not raw ICE troubleshooting.

## Troubleshooting order

1. Confirm `/readyz`, WSS signaling, exact origin, and the expected public or approval-required admission state.
2. Confirm both peers received a non-expired ICE configuration.
3. Check the candidate path. `Determining` during ICE setup is normal; persistent `Unavailable` suggests no selected pair statistic.
4. For direct failures, verify coturn DNS, `3478/udp`, `3478/tcp`, external-IP mapping, and the entire relay UDP range from a remote network.
5. For video without motion, compare requested profile with measured FPS and inspect browser CPU/hardware encoder pressure.
6. For one poor viewer, restart only that viewer's ICE. Do not lower other viewers unless their independent stats require it.
7. For signaling reconnects with healthy media, allow the resume grace period; media should remain open.

Browser `chrome://webrtc-internals` or equivalent tools contain raw IPs, ICE candidates, and SDP. Treat exports as sensitive and do not attach them to public issues.

## Forced-relay acceptance test

The regular Playwright suite skips the relay test. On a Linux test host with reachable coturn, provide the same TURN settings used by the server and run:

```bash
export TURN_HOST=turn.example.com
export TURN_REALM=turn.example.com
export TURN_EXTERNAL_IP=203.0.113.10
export TURN_SHARED_SECRET='test-secret-at-least-32-characters-long'
export ROOM_TOKEN_HMAC_KEY='test-room-key-at-least-32-characters-long'
export RESUME_TOKEN_HMAC_KEY='test-resume-key-at-least-32-characters-long'
docker compose -f deploy/compose.yaml up -d coturn
cd web
pnpm playwright:relay
```

The test build enables a guarded `iceTransportPolicy: relay` in both browser peers, creates a real room through the Rust server, approves the viewer, waits for real video frames, and requires both UIs to classify the selected candidate pair as TURN relay. The override is available only in Vite test mode. Production builds reject synthetic-capture configuration and scan the emitted bundle for its marker.

When the test server is not on the TURN host, `TURN_EXTERNAL_IP` still belongs to coturn, while the Rust process needs `TURN_HOST` and the matching `TURN_SHARED_SECRET`. If coturn is behind NAT, set `TURN_PRIVATE_IP` correctly. Run the browsers from a network that can reach the advertised address; loopback-only testing does not validate public relay behavior.

## ICE and media limitations

- Capture constraints are requests. Browsers may reduce resolution or FPS.
- Candidate classification depends on standards-based stats that vary by engine/version.
- A WebSocket reconnect does not repair an already failed ICE path; ICE restart does.
- TURN is relay-only and cannot fix insufficient presenter encoder capacity or insufficient VPS bandwidth.
- TCP/TLS relay can connect through restrictive networks but is usually less resilient than UDP for live media.
