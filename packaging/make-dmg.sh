#!/bin/sh
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/target/Wiflow.app"
DMG="$ROOT/target/Wiflow-0.1.0-arm64.dmg"
[ -d "$APP" ] || "$ROOT/packaging/build-app.sh"
rm -f "$DMG"
hdiutil create -volname Wiflow -srcfolder "$APP" -ov -format UDZO "$DMG"
echo "built $DMG"
