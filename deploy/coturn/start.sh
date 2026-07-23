#!/bin/sh
set -eu

: "${TURN_REALM:?TURN_REALM is required}"
: "${TURN_SHARED_SECRET:?TURN_SHARED_SECRET is required}"
: "${TURN_EXTERNAL_IP:?TURN_EXTERNAL_IP is required}"
: "${TURN_PORT:=3478}"
: "${TURNS_PORT:=5349}"
: "${TURN_RELAY_MIN_PORT:=49160}"
: "${TURN_RELAY_MAX_PORT:=49260}"

runtime_config=/tmp/turnserver-runtime.conf
cp /etc/coturn/turnserver.conf "$runtime_config"
{
  printf 'realm=%s\n' "$TURN_REALM"
  printf 'static-auth-secret=%s\n' "$TURN_SHARED_SECRET"
  printf 'listening-port=%s\n' "$TURN_PORT"
  printf 'tls-listening-port=%s\n' "$TURNS_PORT"
  printf 'min-port=%s\n' "$TURN_RELAY_MIN_PORT"
  printf 'max-port=%s\n' "$TURN_RELAY_MAX_PORT"
  if [ -n "${TURN_PRIVATE_IP:-}" ]; then
    printf 'external-ip=%s/%s\n' "$TURN_EXTERNAL_IP" "$TURN_PRIVATE_IP"
  else
    printf 'external-ip=%s\n' "$TURN_EXTERNAL_IP"
  fi
  if [ -n "${TURN_TLS_CERT_PATH:-}" ] && [ -n "${TURN_TLS_KEY_PATH:-}" ]; then
    printf 'cert=%s\n' "$TURN_TLS_CERT_PATH"
    printf 'pkey=%s\n' "$TURN_TLS_KEY_PATH"
  else
    printf 'no-tls\n'
    printf 'no-dtls\n'
  fi
} >> "$runtime_config"

exec turnserver -c "$runtime_config"
