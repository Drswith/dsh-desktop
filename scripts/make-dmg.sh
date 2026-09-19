#!/usr/bin/env bash
# Package build/<APP_NAME>.app into a drag-to-Applications disk image.
source "$(dirname "$0")/config.sh"

[ -d "$APP_BUNDLE" ] || die "missing $APP_BUNDLE; run scripts/build-app.sh first"
STAGE="$BUILD_DIR/dmg-stage"
DMG="$BUILD_DIR/$(echo "$APP_NAME" | tr ' ' '-')-$APP_VERSION-$ARCH.dmg"
rm -rf "$STAGE" "$DMG"
mkdir -p "$STAGE"
ditto "$APP_BUNDLE" "$STAGE/$APP_NAME.app"
ln -s /Applications "$STAGE/Applications"
log "creating $(basename "$DMG")"
hdiutil create -quiet -volname "$APP_NAME" -srcfolder "$STAGE" -ov -format UDZO "$DMG"
rm -rf "$STAGE"
# A shared image should be traceable to one clean commit and an explicit build number.
if [ "$BUILD_NUMBER_SOURCE" != explicit ]; then
  log "warning: build number $BUILD_NUMBER is the local commit count; pass BUILD_NUMBER for images you share"
fi
case "$GIT_COMMIT" in *-dirty|unknown) log "warning: built from $GIT_COMMIT, not a clean commit" ;; esac
log "disk image ready: $DMG ($(du -h "$DMG" | awk '{ print $1 }'))"
