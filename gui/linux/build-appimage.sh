#!/bin/sh
# Build "MEGA65 Studio" as a Linux AppImage (Studio plus m65rs inside).
# Usage: gui/linux/build-appimage.sh OUT.AppImage
# Needs appimagetool on PATH, or APPIMAGETOOL=/path/to/appimagetool.
set -eu
cd "$(dirname "$0")/../.."

OUT="$1"
APPDIR="target/linux/MEGA65_Studio.AppDir"

cargo build --release --locked -p m65-studio -p mega65

rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin"
cp target/release/m65-studio target/release/m65rs "$APPDIR/usr/bin/"
python3 gui/icon.py "$APPDIR/m65-studio.png"

cat > "$APPDIR/m65-studio.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=MEGA65 Studio
Comment=MEGA65 development and debugging workstation
Exec=m65-studio
Icon=m65-studio
Categories=Development;
DESKTOP

cat > "$APPDIR/AppRun" <<'APPRUN'
#!/bin/sh
exec "$(dirname "$(readlink -f "$0")")/usr/bin/m65-studio" "$@"
APPRUN
chmod +x "$APPDIR/AppRun"

# APPIMAGE_EXTRACT_AND_RUN lets appimagetool (itself an AppImage) run without FUSE.
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "${APPIMAGETOOL:-appimagetool}" "$APPDIR" "$OUT"
echo "built $OUT"
