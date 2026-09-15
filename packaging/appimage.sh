#!/usr/bin/env bash
# Build LimeLight-<arch>.AppImage from release binaries.
#
#   packaging/appimage.sh [release-dir]      (default: helper/target/release)
#
# Uses linuxdeploy to collect the shared libraries the GUI links against
# (GL/Mesa and glibc are excluded by its blacklist, as they must be).
# Works without FUSE (APPIMAGE_EXTRACT_AND_RUN), so it runs in CI containers.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN=${1:-$ROOT/helper/target/release}
APP_ID=io.github.chimi6.limelight-linux-elgato-lights-controller
ARCH=$(uname -m)
WORK=${APPIMAGE_WORK:-$ROOT/build-appimage}
VERSION=$(grep -m1 '^version' "$ROOT/helper/Cargo.toml" | cut -d'"' -f2)
OUT=${APPIMAGE_OUT:-$ROOT/LimeLight-$ARCH.AppImage}

for f in keylight-gui keylightd; do
  [ -x "$BIN/$f" ] || { echo "missing $BIN/$f (run: cargo build --release -p keylightd -p keylight-gui)" >&2; exit 1; }
done

rm -rf "$WORK"
APPDIR="$WORK/AppDir"
mkdir -p "$APPDIR/usr/bin" \
         "$APPDIR/usr/share/applications" \
         "$APPDIR/usr/share/icons/hicolor/256x256/apps" \
         "$APPDIR/usr/share/icons/hicolor/512x512/apps" \
         "$APPDIR/usr/share/metainfo"

cp "$BIN/keylight-gui" "$APPDIR/usr/bin/limelight"
cp "$BIN/keylightd"    "$APPDIR/usr/bin/keylightd"
cp "$ROOT/flatpak/$APP_ID.desktop" "$APPDIR/usr/share/applications/$APP_ID.desktop"
cp "$ROOT/flatpak/$APP_ID.metainfo.xml" "$APPDIR/usr/share/metainfo/$APP_ID.metainfo.xml"
cp "$ROOT/public/Limecon-256.png" "$APPDIR/usr/share/icons/hicolor/256x256/apps/$APP_ID.png"
cp "$ROOT/public/Limecon-512.png" "$APPDIR/usr/share/icons/hicolor/512x512/apps/$APP_ID.png"

LD="$WORK/linuxdeploy-$ARCH.AppImage"
curl -fsSL -o "$LD" "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-$ARCH.AppImage"
chmod +x "$LD"

export APPIMAGE_EXTRACT_AND_RUN=1
export VERSION
export OUTPUT="$OUT"
"$LD" --appdir "$APPDIR" \
  --desktop-file "$APPDIR/usr/share/applications/$APP_ID.desktop" \
  --icon-file "$APPDIR/usr/share/icons/hicolor/512x512/apps/$APP_ID.png" \
  --executable "$APPDIR/usr/bin/limelight" \
  --executable "$APPDIR/usr/bin/keylightd" \
  --output appimage

chmod +x "$OUT"
echo "built $OUT"
