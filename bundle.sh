#!/usr/bin/env bash
# Builds Mica.app.
#
# Signing is ad-hoc (`-`), which is enough to run locally and is deliberately
# *not* a real identity: signing with a Developer ID and notarising touches
# credentials, and per the project's rules that needs explicit approval before
# it happens. See BUILD-ORDER.md, Phase 10.
set -euo pipefail

PROFILE="${1:-release}"
ROOT="$(cd "$(dirname "$0")" && pwd)"
APP="$ROOT/target/$PROFILE/Mica.app"

cargo build --profile "$PROFILE" -p mica-shell --bin mica

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$ROOT/resources/Info.plist" "$APP/Contents/Info.plist"
cp "$ROOT/target/$PROFILE/mica" "$APP/Contents/MacOS/mica"

# The shaders are compiled by build.rs into OUT_DIR; find the newest one rather
# than guessing the hash cargo picked.
METALLIB="$(find "$ROOT/target/$PROFILE/build" -name default.metallib -print0 \
    | xargs -0 ls -t 2>/dev/null | head -n1)"
[ -n "$METALLIB" ] || { echo "bundle: default.metallib not found — did build.rs run?" >&2; exit 1; }
cp "$METALLIB" "$APP/Contents/Resources/default.metallib"

printf 'APPL????' > "$APP/Contents/PkgInfo"

codesign --force --sign - \
    --options runtime \
    --entitlements "$ROOT/resources/Mica.entitlements" \
    "$APP" >/dev/null 2>&1 \
  || codesign --force --sign - --entitlements "$ROOT/resources/Mica.entitlements" "$APP"

echo "built $APP"
codesign -dv "$APP" 2>&1 | sed -n '1,6p'
