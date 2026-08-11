#!/usr/bin/env bash
# Render the GUI to PNGs, one per scene, without a display server grabbing focus.
#
# The app draws itself to an offscreen framebuffer (CLARITY_SHOT) after routing
# to a scene (CLARITY_SCENE), then exits. This is how the UI is verified during
# development — the same pixels the GPU would show, captured deterministically.
#
# Usage:
#   ./shot.sh                       # every scene below, into ./shots/
#   ./shot.sh room friends          # just these, into ./shots/
#   OUT=/tmp/x ./shot.sh home.palette
#
# Scene tokens (dot/comma/space separated, order-independent):
#   screens   home room friends settings onboarding
#   overlays  palette theatre motion text
# e.g. "room.theatre", "home.palette"

set -euo pipefail
cd "$(dirname "$0")"

OUT="${OUT:-shots}"
BIN="${BIN:-../../target/debug/clarity-gui}"
SCENES=("$@")
if [ ${#SCENES[@]} -eq 0 ]; then
  SCENES=(home home.palette room friends settings onboarding)
fi

[ -x "$BIN" ] || cargo build -p clarity-desktop
mkdir -p "$OUT"

for scene in "${SCENES[@]}"; do
  file="$OUT/${scene}.png"
  rm -f "$file"
  CLARITY_SHOT="$file" CLARITY_SCENE="$scene" timeout 20 "$BIN" >/dev/null 2>&1 || true
  if [ -s "$file" ]; then
    printf '  %-26s %s\n' "$scene" "$file"
  else
    printf '  %-26s FAILED\n' "$scene" >&2
  fi
done
