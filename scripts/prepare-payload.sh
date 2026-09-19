#!/usr/bin/env bash
# Build the runtime payload the app extracts on first launch:
#   runtime.tar.gz = node/ (official Node.js) + pnpm/ + app/ (the committed runtime
#   project installed from its lockfile) + bin/ (dsh and pnpm shims), plus manifest.json.
# The install mirrors the official desktop seed: the pinned pnpm runs under the
# bundled Node with an isolated store and config, a hoisted node_modules, and only
# the reviewed dependency builds allowed; --frozen-lockfile makes it reproducible.
source "$(dirname "$0")/config.sh"
source "$(dirname "$0")/signing.sh"
source "$(dirname "$0")/toolchain.sh"
check_signing_identity
check_locked_version

MANIFEST="$PAYLOAD_DIR/manifest.json"
PROJECT_FILES=("$RUNTIME_PROJECT/package.json" "$RUNTIME_PROJECT/pnpm-workspace.yaml" "$RUNTIME_PROJECT/pnpm-lock.yaml")
for file in "${PROJECT_FILES[@]}"; do [ -f "$file" ] || die "missing $file; run: make lock"; done
PROJECT_HASH="$(cat "${PROJECT_FILES[@]}" | shasum -a 256 | cut -c1-16)"
IDENTITY="dsh=$LOCKED_DSH_VERSION lock=$PROJECT_HASH node=$NODE_VERSION pnpm=$PNPM_VERSION arch=$ARCH sign=$(signing_label)"

if [ "${FORCE:-0}" != 1 ] && [ -f "$MANIFEST" ] && [ -f "$PAYLOAD_DIR/build-identity" ] \
  && [ "$(cat "$PAYLOAD_DIR/build-identity")" = "$IDENTITY" ]; then
  log "payload up to date ($IDENTITY); FORCE=1 rebuilds it"
  exit 0
fi

WORK="$BUILD_DIR/payload-work/darwin-$ARCH"
RUNTIME="$WORK/runtime"
rm -rf "$WORK"
mkdir -p "$RUNTIME"

# 1-2. Node.js and pnpm, verified.
install_node "$RUNTIME"
install_pnpm "$RUNTIME"

# 3. The committed runtime project, installed exactly as locked.
mkdir -p "$RUNTIME/app"
cp "${PROJECT_FILES[@]}" "$RUNTIME/app/"
log "installing @deepseek-ai/dsh@$LOCKED_DSH_VERSION from runtime/pnpm-lock.yaml with pnpm $PNPM_VERSION on Node $NODE_VERSION ($ARCH)"
run_pnpm "$RUNTIME" "$RUNTIME/app" "$WORK/pnpm-state" install --prod --frozen-lockfile --reporter=append-only
# An assignment keeps the check's exit status (a `log "$(…)"` would mask it).
platform_report="$(check_platform_packages "$RUNTIME" "$RUNTIME/app")" || die "the install is incomplete; rerun to retry the downloads"
log "$platform_report"

# 4. Drop prebuilt binaries for other platforms and the other Mac architecture.
OTHER_DARWIN_ARCH="$([ "$NODE_ARCH" = arm64 ] && echo x64 || echo arm64)"
find "$RUNTIME/app/node_modules" -type d -path '*/prebuilds/*' \
  \( -name 'win32-*' -o -name 'linux-*' -o -name "darwin-$OTHER_DARWIN_ARCH" \) -prune -exec rm -rf {} +

# 5. Shims for terminal use: `~/.dsh-launcher/runtime/current/bin/dsh plugin --profile launcher add …`
mkdir -p "$RUNTIME/bin"
write_shim() { # name target-args
  cat > "$RUNTIME/bin/$1" <<EOF
#!/bin/sh
# Resolve symlinks so the shim works when linked from another directory.
self="\$0"
while [ -L "\$self" ]; do
  link="\$(readlink "\$self")"
  case "\$link" in /*) self="\$link" ;; *) self="\$(dirname "\$self")/\$link" ;; esac
done
runtime="\$(cd "\$(dirname "\$self")/.." && pwd -P)"
PATH="\$runtime/bin:\$runtime/node/bin:\$PATH" exec "\$runtime/node/bin/node" $2 "\$@"
EOF
  chmod 755 "$RUNTIME/bin/$1"
}
write_shim dsh '"$runtime/app/node_modules/@deepseek-ai/dsh/lib/bin.js"'
write_shim pnpm '"$runtime/pnpm/bin/pnpm.mjs"'

# 5b. Sign each Mach-O on its own (Node with JIT entitlements), as the official desktop does.
sign_runtime_tree "$RUNTIME"

# 6. Verify the installed CLI, then boot the signed runtime once on a scratch home;
#    this also proves Node runs under the hardened runtime with its entitlements.
installed="$("$RUNTIME/bin/dsh" --version)"
[ "$installed" = "$LOCKED_DSH_VERSION" ] || die "installed dsh reports $installed, expected $LOCKED_DSH_VERSION"
if [ "${SKIP_SMOKE:-0}" != 1 ]; then
  "$(dirname "$0")/smoke-runtime.sh" "$RUNTIME"
fi

# 7. Archive + manifest.
mkdir -p "$PAYLOAD_DIR"
rm -f "$PAYLOAD_DIR"/runtime.tar.gz* "$MANIFEST" "$PAYLOAD_DIR/build-identity" "$PAYLOAD_DIR/pnpm-lock.yaml"
log "archiving runtime ($(du -sh "$RUNTIME" | awk '{ print $1 }') unpacked)"
COPYFILE_DISABLE=1 tar --no-mac-metadata -C "$RUNTIME" -czf "$PAYLOAD_DIR/runtime.tar.gz.part" bin node pnpm app
mv "$PAYLOAD_DIR/runtime.tar.gz.part" "$PAYLOAD_DIR/runtime.tar.gz"
sha="$(shasum -a 256 "$PAYLOAD_DIR/runtime.tar.gz" | awk '{ print $1 }')"
cat > "$MANIFEST" <<EOF
{
  "schemaVersion": 1,
  "dshVersion": "$LOCKED_DSH_VERSION",
  "nodeVersion": "$NODE_VERSION",
  "pnpmVersion": "$PNPM_VERSION",
  "platform": "darwin",
  "arch": "$ARCH",
  "archive": "runtime.tar.gz",
  "archiveSHA256": "$sha",
  "createdAt": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
EOF
echo "$IDENTITY" > "$PAYLOAD_DIR/build-identity"
rm -rf "$WORK"
log "payload ready: $PAYLOAD_DIR ($(du -h "$PAYLOAD_DIR/runtime.tar.gz" | awk '{ print $1 }'), sha256 ${sha:0:12}…)"
