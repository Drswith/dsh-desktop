#!/usr/bin/env bash
# Compile the Swift shell and assemble build/<APP_NAME>.app:
#   Contents/MacOS/DSHLauncher           native menu bar launcher
#   Contents/Resources/payload/          runtime.aar + manifest.json (from prepare-payload.sh)
#   Contents/Resources/*.lproj, icons    UI resources
source "$(dirname "$0")/config.sh"

log "compiling $EXECUTABLE_NAME (release, $ARCH)"
swift build --package-path "$ROOT" -c release --arch "$ARCH" --product "$EXECUTABLE_NAME"
BIN_DIR="$(swift build --package-path "$ROOT" -c release --arch "$ARCH" --show-bin-path)"

MANIFEST="$PAYLOAD_DIR/manifest.json"
if [ -f "$MANIFEST" ]; then
  payload_arch="$(sed -n 's/^  "arch": "\(.*\)",$/\1/p' "$MANIFEST")"
  [ "$payload_arch" = "$ARCH" ] || die "payload arch $payload_arch does not match ARCH=$ARCH"
  BUNDLED_DSH_VERSION="$(sed -n 's/^  "dshVersion": "\(.*\)",$/\1/p' "$MANIFEST")"
elif [ "${ALLOW_NO_PAYLOAD:-0}" = 1 ]; then
  log "warning: no payload; the app will need \"runtime\" in ~/$HOME_DIR_NAME/config.json"
  BUNDLED_DSH_VERSION="external"
else
  die "no payload at $PAYLOAD_DIR; run scripts/prepare-payload.sh (or set ALLOW_NO_PAYLOAD=1)"
fi

CONTENTS="$APP_BUNDLE/Contents"
rm -rf "$APP_BUNDLE"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"
cp "$BIN_DIR/$EXECUTABLE_NAME" "$CONTENTS/MacOS/$EXECUTABLE_NAME"
printf 'APPL????' > "$CONTENTS/PkgInfo"

sed \
  -e "s|@APP_NAME@|$APP_NAME|g" \
  -e "s|@EXECUTABLE_NAME@|$EXECUTABLE_NAME|g" \
  -e "s|@BUNDLE_ID@|$BUNDLE_ID|g" \
  -e "s|@MARKETING_VERSION@|$MARKETING_VERSION|g" \
  -e "s|@APP_VERSION@|$APP_VERSION|g" \
  -e "s|@BUILD_NUMBER@|$BUILD_NUMBER|g" \
  -e "s|@URL_SCHEME@|$URL_SCHEME|g" \
  -e "s|@HOME_DIR_NAME@|$HOME_DIR_NAME|g" \
  -e "s|@DSH_PROFILE@|$DSH_PROFILE|g" \
  -e "s|@DEFAULT_PORT@|$DEFAULT_PORT|g" \
  -e "s|@DSH_VERSION@|$BUNDLED_DSH_VERSION|g" \
  -e "s|@COPYRIGHT@|$COPYRIGHT|g" \
  -e "s|@GIT_COMMIT@|$GIT_COMMIT|g" \
  -e "s|@BUILD_DATE@|$BUILD_DATE|g" \
  -e "s|@REPO_URL@|$REPO_URL|g" \
  "$ROOT/Resources/Info.plist.in" > "$CONTENTS/Info.plist"
plutil -lint -s "$CONTENTS/Info.plist"

# AppIcon.icns from the 1024px source, cached until the source changes.
ICON_SRC="$ROOT/Resources/Icons/AppIcon.png"
ICNS="$BUILD_DIR/AppIcon.icns"
if [ ! -f "$ICNS" ] || [ "$ICON_SRC" -nt "$ICNS" ]; then
  log "rendering AppIcon.icns"
  ICONSET="$BUILD_DIR/AppIcon.iconset"
  rm -rf "$ICONSET"
  mkdir -p "$ICONSET"
  for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$ICON_SRC" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" "$ICON_SRC" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
  done
  iconutil -c icns "$ICONSET" -o "$ICNS"
  rm -rf "$ICONSET"
fi
cp "$ICNS" "$CONTENTS/Resources/AppIcon.icns"
cp "$ROOT/Resources/Icons/MenuBarIconTemplate.png" "$ROOT/Resources/Icons/MenuBarIconTemplate@2x.png" "$CONTENTS/Resources/"
cp -R "$ROOT/Resources/en.lproj" "$ROOT/Resources/zh-Hans.lproj" "$CONTENTS/Resources/"

if [ -f "$MANIFEST" ]; then
  mkdir -p "$CONTENTS/Resources/payload"
  cp -c "$MANIFEST" "$PAYLOAD_DIR/runtime.aar" "$CONTENTS/Resources/payload/" 2>/dev/null \
    || cp "$MANIFEST" "$PAYLOAD_DIR/runtime.aar" "$CONTENTS/Resources/payload/"
fi

# Only the app is signed. runtime.aar is sealed data to this signature, and the
# code inside keeps the signatures it ships with: Node.js's own Developer ID, and
# the linker's ad hoc signature on arm64 native modules.
if [ "$CODESIGN_IDENTITY" = - ]; then signer="ad hoc"; else signer="$CODESIGN_IDENTITY"; fi
log "signing $APP_NAME.app ($signer)"
codesign --force --sign "$CODESIGN_IDENTITY" --options runtime "$APP_BUNDLE"
codesign --verify --strict "$APP_BUNDLE"

log "built $APP_BUNDLE ($(du -sh "$APP_BUNDLE" | awk '{ print $1 }'), $APP_VERSION ($BUILD_NUMBER), commit ${GIT_COMMIT:0:12}, dsh $BUNDLED_DSH_VERSION, $ARCH)"
