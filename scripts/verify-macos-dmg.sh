#!/bin/sh
# Verify that the app inside a built .dmg has a valid, sealed code signature.
# Without this check a packaging change can silently ship an app whose
# signature covers nothing, which macOS reports to users as "damaged".
# Usage: scripts/verify-macos-dmg.sh dist/Writely_0.1.0_macos_arm64.dmg
set -eu

DMG="${1:-}"
if [ -z "$DMG" ] || [ ! -f "$DMG" ]; then
  echo "verify-macos-dmg: usage: $0 <path-to-dmg>" >&2
  exit 1
fi

MOUNT="$(mktemp -d)"
cleanup() {
  hdiutil detach "$MOUNT" -quiet 2>/dev/null || true
  rmdir "$MOUNT" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

hdiutil attach -nobrowse -readonly -mountpoint "$MOUNT" "$DMG" >/dev/null

APP="$(find "$MOUNT" -maxdepth 1 -name '*.app' -type d | head -n 1)"
if [ -z "$APP" ]; then
  echo "verify-macos-dmg: no .app found in $DMG" >&2
  exit 1
fi

# Fails (exit 1) on the old broken bundles: "code has no resources but
# signature indicates they must be present". An ad-hoc signature passes:
# Gatekeeper still shows a one-time "Open Anyway" prompt because the app is
# not notarized, but it is no longer reported as damaged.
codesign --verify --strict "$APP"
echo "verify-macos-dmg: OK: $APP in $DMG has a valid signature"
