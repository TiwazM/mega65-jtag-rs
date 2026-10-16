#!/bin/sh
# Build "MEGA65 Studio.app" for macOS: release build, bundle, icon, ad-hoc signature.
# Usage: gui/macos/build-app.sh [--install]   (--install copies it to ~/Applications)
set -eu
cd "$(dirname "$0")/../.."

APP_NAME="MEGA65 Studio"
BUNDLE_ID="org.mega65rs.studio"
VERSION=$(grep -m1 '^version' gui/Cargo.toml | sed 's/.*"\(.*\)".*/\1/')
OUT="target/macos/$APP_NAME.app"

cargo build --release -p m65-studio

rm -rf "$OUT"
mkdir -p "$OUT/Contents/MacOS" "$OUT/Contents/Resources"
cp target/release/m65-studio "$OUT/Contents/MacOS/m65-studio"

cat > "$OUT/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$APP_NAME</string>
  <key>CFBundleDisplayName</key><string>$APP_NAME</string>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleExecutable</key><string>m65-studio</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>Commodore program</string>
      <key>CFBundleTypeExtensions</key><array><string>prg</string></array>
      <key>CFBundleTypeRole</key><string>Viewer</string>
    </dict>
  </array>
</dict>
</plist>
EOF

# Icon: the MEGA65 boot-screen stripes on a dark tile, drawn without any image
# library, then turned into an .icns with the system tools.
ICONSET="target/macos/AppIcon.iconset"
rm -rf "$ICONSET"
mkdir -p "$ICONSET"
python3 - "$ICONSET/icon_512x512@2x.png" <<'PY'
import struct, sys, zlib
size = 1024
bg = (0x1c, 0x1e, 0x2a)
stripes = [(0xff, 0x30, 0x30), (0xff, 0x80, 0x20), (0xff, 0xe0, 0x30), (0x30, 0xd0, 0x40), (0x30, 0xa0, 0xff), (0xc0, 0x40, 0xff)]
rows = []
r_corner = 180
for y in range(size):
    row = bytearray()
    for x in range(size):
        # rounded square mask
        dx = max(r_corner - x, x - (size - 1 - r_corner), 0)
        dy = max(r_corner - y, y - (size - 1 - r_corner), 0)
        if dx * dx + dy * dy > r_corner * r_corner:
            row += b"\0\0\0\0"
            continue
        c = bg
        # six diagonal stripes like the boot banner, from the lower left
        band = (y - 300) // 70
        if 0 <= band < 6:
            x0 = 150 + (5 - band) * 55
            if x0 <= x < x0 + 520:
                c = stripes[band]
        row += bytes(c) + b"\xff"
    rows.append(b"\0" + bytes(row))
raw = b"".join(rows)
def chunk(t, d):
    return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xffffffff)
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")
open(sys.argv[1], "wb").write(png)
PY
for s in 16 32 128 256 512; do
  sips -z $s $s "$ICONSET/icon_512x512@2x.png" --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
  d=$((s * 2))
  [ $s -ne 512 ] && sips -z $d $d "$ICONSET/icon_512x512@2x.png" --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$OUT/Contents/Resources/AppIcon.icns"

# Ad-hoc signature so macOS runs it without complaint on this machine.
codesign --force --deep --sign - "$OUT" >/dev/null
echo "built $OUT"

if [ "${1:-}" = "--install" ]; then
  mkdir -p "$HOME/Applications"
  rm -rf "$HOME/Applications/$APP_NAME.app"
  cp -R "$OUT" "$HOME/Applications/"
  echo "installed to $HOME/Applications/$APP_NAME.app"
fi
