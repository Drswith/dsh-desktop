#!/usr/bin/env bash
# The pinned Node.js and pnpm, downloaded once into .cache, verified, and run with
# an isolated configuration. Sourced after config.sh by prepare-payload.sh and
# update-lock.sh.

DOWNLOADS="$CACHE_DIR/downloads"

fetch() { # url dest
  [ -s "$2" ] && return 0
  mkdir -p "$(dirname "$2")"
  log "downloading $1"
  curl -fL --retry 3 --progress-bar "$1" -o "$2.part"
  mv "$2.part" "$2"
}

# install_node <runtime-dir>: the official Node.js binary (verified against the
# release SHASUMS256.txt) as <runtime-dir>/node/bin/node, plus its LICENSE.
install_node() {
  local runtime="$1" dist tarball sums expected actual extract
  dist="node-v$NODE_VERSION-darwin-$NODE_ARCH"
  tarball="$DOWNLOADS/$dist.tar.gz"
  sums="$DOWNLOADS/node-v$NODE_VERSION-SHASUMS256.txt"
  fetch "$NODE_DIST_URL/v$NODE_VERSION/SHASUMS256.txt" "$sums"
  fetch "$NODE_DIST_URL/v$NODE_VERSION/$dist.tar.gz" "$tarball"
  expected="$(awk -v file="$dist.tar.gz" '$2 == file { print $1 }' "$sums")"
  actual="$(shasum -a 256 "$tarball" | awk '{ print $1 }')"
  if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
    rm -f "$tarball"
    die "Node.js checksum mismatch for $dist"
  fi
  extract="$(mktemp -d "${TMPDIR:-/tmp}/dsh-launcher-node.XXXXXX")"
  tar -xzf "$tarball" -C "$extract"
  mkdir -p "$runtime/node/bin"
  cp "$extract/$dist/bin/node" "$runtime/node/bin/node"
  cp "$extract/$dist/LICENSE" "$runtime/node/LICENSE"
  rm -rf "$extract"
  [ "$("$runtime/node/bin/node" --version)" = "v$NODE_VERSION" ] || die "bundled Node does not report v$NODE_VERSION"
}

# install_pnpm <runtime-dir>: the pnpm npm package (verified against the registry's
# sha512 integrity) as <runtime-dir>/pnpm. Needs install_node first.
install_pnpm() {
  local runtime="$1" tarball integrity
  tarball="$DOWNLOADS/pnpm-$PNPM_VERSION.tgz"
  fetch "${NPM_REGISTRY}pnpm/-/pnpm-$PNPM_VERSION.tgz" "$tarball"
  integrity="$(curl -fsSL "${NPM_REGISTRY}pnpm/$PNPM_VERSION" | "$runtime/node/bin/node" -e \
    'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>console.log(JSON.parse(s).dist.integrity))')"
  if [ "$integrity" != "sha512-$(openssl dgst -sha512 -binary "$tarball" | base64)" ]; then
    rm -f "$tarball"
    die "pnpm $PNPM_VERSION integrity mismatch"
  fi
  mkdir -p "$runtime/pnpm"
  tar -xzf "$tarball" -C "$runtime/pnpm" --strip-components=1
}

# run_pnpm <runtime-dir> <project-dir> <state-dir> <pnpm args...>
# Runs the bundled pnpm on the bundled Node in a clean environment: fixed registry,
# the shared cache store, and no user npm/pnpm configuration.
run_pnpm() {
  local runtime="$1" project="$2" state="$3"
  shift 3
  mkdir -p "$state/config"
  : > "$state/config/npmrc"
  (
    cd "$project"
    env -i \
      HOME="$HOME" \
      PATH="$runtime/node/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
      LANG="${LANG:-en_US.UTF-8}" \
      XDG_CACHE_HOME="$state/cache" \
      XDG_CONFIG_HOME="$state/config" \
      XDG_STATE_HOME="$state/state" \
      NPM_CONFIG_USERCONFIG="$state/config/npmrc" \
      "$runtime/node/bin/node" "$runtime/pnpm/bin/pnpm.mjs" \
        --config.registry="$NPM_REGISTRY" \
        --config.store-dir="$CACHE_DIR/pnpm-store" \
        --config.enable-global-virtual-store=false \
        --config.userconfig="$state/config/npmrc" \
        --config.package-import-method=clone-or-copy \
        --config.update-notifier=false \
        "$@"
  )
}
