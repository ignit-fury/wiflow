#!/bin/sh
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/target/Wiflow.app"
cargo build --release --manifest-path "$ROOT/Cargo.toml"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$ROOT/target/release/wiflow-dictation" "$APP/Contents/MacOS/"
cp "$ROOT/packaging/Info.plist" "$APP/Contents/"
python3 - "$APP/Contents/Resources/icon.png" <<'EOF'
import struct, sys, zlib
w = h = 32
raw = bytearray()
for y in range(h):
    for x in range(w):
        dx, dy = x - 16, y - 16
        r, g, b = ((230, 40, 40) if dx*dx + dy*dy <= 49 else (24, 24, 24))
        raw += bytes([r, g, b])
def chunk(t, d):
    c = struct.pack(">I", len(d)) + t + d
    return c + struct.pack(">I", zlib.crc32(t + d) & 0xffffffff)
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
stride = w * 3
scan = b"".join(b"\x00" + raw[i:i+stride] for i in range(0, len(raw), stride))
png += chunk(b"IDAT", zlib.compress(bytes(scan))) + chunk(b"IEND", b"")
open(sys.argv[1], "wb").write(png)
EOF
codesign --force --deep --sign - "$APP"
echo "built $APP"
