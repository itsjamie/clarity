# Single-VPS deployment

## Host requirements

Use a public Linux VPS with Docker Engine, Compose v2, a stable public IPv4 address, enough CPU for encrypted packet forwarding, and enough network quota for the intended relay bitrate. The host must expose UDP; platforms limited to HTTP/TCP cannot run a complete TURN deployment.

The supplied Compose file uses host networking for coturn. This is deliberate: it avoids publishing a large relay-port range through Docker's userland/NAT layer and gives ICE candidates predictable addresses. It also removes container-network isolation for coturn, so run this stack only on a dedicated, hardened Linux host and bind no unrelated coturn services.

The application container is exposed only on the private Compose network. The
Compose file sets `TRUSTED_PROXY_HOPS=1`, allowing per-client rate limits to use
Caddy's immediate client address. Direct deployments must leave this at `0`;
only raise it when every hop between the public listener and the application is
controlled and trusted.

## DNS

Create these records before starting Caddy:

| Record | Example | Target |
| --- | --- | --- |
| `A` | `share.example.com` | VPS public IPv4 |
| `A` | `turn.example.com` | the same VPS public IPv4 |

Set `APP_DOMAIN`/`PUBLIC_BASE_URL` to the share name and `TURN_HOST`/`TURN_REALM` to the TURN name. Add `AAAA` records only after configuring and testing IPv6 listeners, external addresses, and firewall rules; a broken IPv6 path is worse than no AAAA record.

## Firewall

Allow at minimum:

- `80/tcp` and `443/tcp` for Caddy and WSS.
- `3478/udp` and `3478/tcp` for STUN/TURN.
- `5349/tcp` only when TURNS is configured.
- `49160-49260/udp`, or the exact `TURN_RELAY_MIN_PORT` through `TURN_RELAY_MAX_PORT` range.

The default range provides 101 relay ports. Capacity depends on traffic shape and address family; widen it for sustained concurrency, then update both the firewall and environment atomically. TCP relay candidates use the allocated relay range as well when negotiated, so permit the configured TCP range if your policy enables that mode. Restrict SSH separately to trusted administration sources.

## Configure secrets and addresses

```bash
cp .env.example .env
chmod 600 .env
```

Replace every `REPLACE_WITH` value with an independent random value. Set:

- `PUBLIC_BASE_URL=https://share.example.com`
- `ALLOWED_ORIGINS=https://share.example.com`
- `APP_DOMAIN=share.example.com`
- `TURN_HOST=turn.example.com`
- `TURN_REALM=turn.example.com`
- `TURN_EXTERNAL_IP` to the public IPv4 address.

If the VPS has the public address directly on an interface, leave `TURN_PRIVATE_IP` empty. If the provider maps a public address onto a private interface, set `TURN_PRIVATE_IP` to that interface address so coturn emits `external-ip=PUBLIC/PRIVATE`. Verify the mapping with a remote client; do not guess it from container-local addresses.

## TLS and TURN transport

Caddy automatically obtains the application certificate for `APP_DOMAIN`. This secures HTTPS and WSS only. TURN UDP/TCP on 3478 uses DTLS-SRTP media encryption negotiated by WebRTC but is not a `turns:` control connection.

TURNS is optional. To enable it, obtain a certificate valid for `TURN_HOST`, mount its certificate and private key read-only into the coturn container, uncomment/update the certificate volume in `deploy/compose.yaml`, and set `TURN_TLS_CERT_PATH` and `TURN_TLS_KEY_PATH` to the in-container paths. The Rust service adds the `turns:` URL only when both variables are present. Ensure the coturn process can read the files and open `5349/tcp`.

## Start and verify

```bash
docker compose -f deploy/compose.yaml pull
docker compose -f deploy/compose.yaml build --pull
docker compose -f deploy/compose.yaml up -d
docker compose -f deploy/compose.yaml ps
docker compose -f deploy/compose.yaml logs --tail=100 app caddy coturn
curl --fail https://share.example.com/healthz
curl --fail https://share.example.com/readyz
```

Create a short-lived room from the UI, join from a different network, approve the viewer, and confirm the quality HUD shows live video and a Direct or TURN relay path. Then execute the forced-relay test described in [WebRTC diagnostics](WEBRTC-DIAGNOSTICS.md#forced-relay-acceptance-test).

## Upgrades and rollback

```bash
docker compose -f deploy/compose.yaml pull
docker compose -f deploy/compose.yaml build --pull
docker compose -f deploy/compose.yaml up -d --remove-orphans
docker image prune
```

An application restart ends active rooms because there is intentionally no database. Announce a maintenance window and let shares end first. Pin reviewed image versions/digests for controlled production environments and retain the previously built application image for rollback. Rollback restores binaries/configuration, not active rooms.

Back up `.env` securely and the Compose/config files. Caddy's `caddy_data` volume contains account and certificate state and should be included in host backups. There is no room/media database to back up. Never copy the TURN shared secret into the frontend or Caddy configuration.

## Operations

Application and proxy logs go to container stdout; production Rust logs are structured JSON. Configure Docker log rotation or a host collector that does not ship sensitive browser payloads. Monitor disk, memory, container restarts, certificate renewal, relay port exhaustion, and aggregate network ingress/egress. `docker stats` and provider bandwidth graphs are useful first checks.

Four independent high-bitrate peers can require roughly four times one stream of presenter upload. If all four relay, the VPS receives the presenter traffic and sends a copy toward each viewer; billing and NIC capacity can become the limiting factors. Coturn adds latency and is not a transcoder.

Symmetric NAT, enterprise firewalls, carrier networks, or UDP blocking often force relay. TURN-over-TCP/TLS improves reachability but can perform poorly under packet loss due to head-of-line blocking. Keep UDP enabled as the preferred relay transport.
