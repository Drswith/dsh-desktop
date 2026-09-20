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
# the shared cache store, and no user npm/pnpm configuration. HTTP(S)_PROXY and
# NO_PROXY are the one exception, so builds behind a proxy reach the registry.
run_pnpm() {
  local runtime="$1" project="$2" state="$3" proxy_env=() proxy_config=()
  shift 3
  local https_proxy_value="${HTTPS_PROXY:-${https_proxy:-}}"
  local http_proxy_value="${HTTP_PROXY:-${http_proxy:-}}"
  local no_proxy_value="${NO_PROXY:-${no_proxy:-}}"
  if [ -n "$https_proxy_value" ]; then
    proxy_env+=("HTTPS_PROXY=$https_proxy_value")
    proxy_config+=("--config.https-proxy=$https_proxy_value")
  fi
  if [ -n "$http_proxy_value" ]; then
    proxy_env+=("HTTP_PROXY=$http_proxy_value")
    proxy_config+=("--config.proxy=$http_proxy_value")
  fi
  if [ -n "$no_proxy_value" ]; then
    proxy_env+=("NO_PROXY=$no_proxy_value")
    proxy_config+=("--config.noproxy=$no_proxy_value")
  fi
  mkdir -p "$state/config"
  # Numbers belong in the config file: --config.<key> passes the raw string through,
  # and pnpm then hands "60000" to APIs that want a number.
  cat > "$state/config/npmrc" <<EOF
fetch-retries=5
fetch-timeout=$PNPM_FETCH_TIMEOUT
EOF
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
      ${proxy_env[@]+"${proxy_env[@]}"} \
      "$runtime/node/bin/node" "$runtime/pnpm/bin/pnpm.mjs" \
        --config.registry="$NPM_REGISTRY" \
        --config.store-dir="$CACHE_DIR/pnpm-store" \
        --config.enable-global-virtual-store=false \
        --config.userconfig="$state/config/npmrc" \
        --config.package-import-method=clone-or-copy \
        --config.update-notifier=false \
        ${proxy_config[@]+"${proxy_config[@]}"} \
        "$@"
  )
}

# check_platform_packages <runtime-dir> <project-dir>
# pnpm skips an optional dependency whose download fails, so a flaky network can
# leave out a native package (sharp's libvips, say) without failing the install.
# Require every lockfile package built for this macOS architecture to be on disk.
check_platform_packages() {
  "$1/node/bin/node" - "$2" "$NODE_ARCH" <<'JS'
const fs = require('node:fs')
const path = require('node:path')
const [project, cpu] = process.argv.slice(2)
const lock = fs.readFileSync(path.join(project, 'pnpm-lock.yaml'), 'utf8')
const packages = lock.slice(lock.indexOf('\npackages:\n'), lock.indexOf('\nsnapshots:\n'))
const list = text => text.split(',').map(item => item.trim().replace(/^'|'$/g, ''))
const expected = new Set()
let name, os, arch
const settle = () => {
  if (name && os?.includes('darwin') && (arch === undefined || arch.includes(cpu))) expected.add(name)
}
for (const line of packages.split('\n')) {
  const head = line.match(/^  '?((?:@[^/'@]+\/)?[^@']+)@[^:]*'?:$/)
  if (head) { settle(); [name, os, arch] = [head[1], undefined, undefined]; continue }
  const osMatch = line.match(/^    os: \[(.*)\]$/)
  if (osMatch) os = list(osMatch[1])
  const cpuMatch = line.match(/^    cpu: \[(.*)\]$/)
  if (cpuMatch) arch = list(cpuMatch[1])
}
settle()
const missing = [...expected].filter(pkg => !fs.existsSync(path.join(project, 'node_modules', pkg, 'package.json')))
if (missing.length > 0) {
  console.error(`missing ${missing.length} of ${expected.size} darwin-${cpu} packages: ${missing.join(', ')}`)
  process.exit(1)
}
console.log(`all ${expected.size} darwin-${cpu} platform packages are installed`)
JS
}
