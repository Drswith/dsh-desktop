#!/usr/bin/env bash
# Compile the Tauri shell and assemble build/<APP_NAME>.app:
#   Contents/MacOS/dsh-launcher         the launcher (its web assets are compiled in)
#   Contents/Resources/payload/         runtime.aar + manifest.json (from prepare-payload.sh)
#   Contents/Resources/AppIcon.icns     rendered from icons/AppIcon.png
# Tauri's own bundler is not used: everything it would do for this app is these
# few lines, and this way the build needs no tool beyond the Rust toolchain.
source "$(dirname "$0")/config.sh"

command -v cargo > /dev/null || die "cargo not found; run through mise (mise run app)"

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

# Build identity, baked into the binary by src-tauri/build.rs.
export DSH_LAUNCHER_VERSION_LABEL="$APP_VERSION"
export DSH_LAUNCHER_BUILD_NUMBER="$BUILD_NUMBER"
export DSH_LAUNCHER_GIT_COMMIT="$GIT_COMMIT"
export DSH_LAUNCHER_BUILD_DATE="$BUILD_DATE"
export DSH_LAUNCHER_REPO_URL="$REPO_URL"
export DSH_LAUNCHER_HOME_DIR_NAME="$HOME_DIR_NAME"
export DSH_LAUNCHER_PROFILE="$DSH_PROFILE"
export DSH_LAUNCHER_URL_SCHEME="$URL_SCHEME"
export DSH_LAUNCHER_DEFAULT_PORT="$DEFAULT_PORT"
export DSH_LAUNCHER_RUNTIME_VERSION="$BUNDLED_DSH_VERSION"

# The identity compiled into the binary has to match the bundle's: it decides the
# app menu's name and the single-instance socket, so a build with another
# BUNDLE_ID (a test copy, say) runs beside an installed one without touching it.
export TAURI_CONFIG="{\"productName\":\"$APP_NAME\",\"version\":\"$MARKETING_VERSION\",\"identifier\":\"$BUNDLE_ID\"}"

log "compiling $EXECUTABLE_NAME (release, $RUST_TARGET)"
rustup target list --installed 2>/dev/null | grep -qx "$RUST_TARGET" || rustup target add "$RUST_TARGET"
(cd "$ROOT/src-tauri" && cargo build --release --locked --target "$RUST_TARGET")
BINARY="$ROOT/src-tauri/target/$RUST_TARGET/release/$EXECUTABLE_NAME"
[ -x "$BINARY" ] || die "cargo produced no $BINARY"

CONTENTS="$APP_BUNDLE/Contents"
rm -rf "$APP_BUNDLE"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"
cp "$BINARY" "$CONTENTS/MacOS/$EXECUTABLE_NAME"
printf 'APPL????' > "$CONTENTS/PkgInfo"

sed \
  -e "s|@APP_NAME@|$APP_NAME|g" \
  -e "s|@EXECUTABLE_NAME@|$EXECUTABLE_NAME|g" \
  -e "s|@BUNDLE_ID@|$BUNDLE_ID|g" \
  -e "s|@MARKETING_VERSION@|$MARKETING_VERSION|g" \
  -e "s|@BUILD_NUMBER@|$BUILD_NUMBER|g" \
  -e "s|@URL_SCHEME@|$URL_SCHEME|g" \
  -e "s|@COPYRIGHT@|$COPYRIGHT|g" \
  "$ROOT/src-tauri/Info.plist.in" > "$CONTENTS/Info.plist"
plutil -lint -s "$CONTENTS/Info.plist"

# AppIcon.icns from the 1024px source, cached until the source changes.
ICON_SRC="$ROOT/src-tauri/icons/AppIcon.png"
ICNS="$BUILD_DIR/AppIcon.icns"
if [ ! -f "$ICNS" ] || [ "$ICON_SRC" -nt "$ICNS" ]; then
  log "rendering AppIcon.icns"
  ICONSET="$BUILD_DIR/AppIcon.iconset"
  rm -rf "$ICONSET"
  mkdir -p "$ICONSET"
  for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$ICON_SRC" --out "$ICONSET/icon_${size}x${size}.png" > /dev/null
    double=$((size * 2))
    sips -z "$double" "$double" "$ICON_SRC" --out "$ICONSET/icon_${size}x${size}@2x.png" > /dev/null
  done
  iconutil -c icns "$ICONSET" -o "$ICNS"
  rm -rf "$ICONSET"
fi
cp "$ICNS" "$CONTENTS/Resources/AppIcon.icns"

if [ -f "$MANIFEST" ]; then
  mkdir -p "$CONTENTS/Resources/payload"
  cp -c "$MANIFEST" "$PAYLOAD_DIR/runtime.aar" "$CONTENTS/Resources/payload/" 2> /dev/null \
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
