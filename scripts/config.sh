#!/usr/bin/env bash
# Shared build settings, sourced by the other scripts. Every value can be
# overridden from the environment, e.g. `ARCH=x86_64 make app`.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HARNESS="$ROOT/deepseek-harness"

die() {
  echo "error: $*" >&2
  exit 1
}

log() {
  echo "==> $*" >&2
}

# --- App identity (kept distinct from the official Electron "DSH Desktop") ---
APP_NAME="${APP_NAME:-DSH Launcher}"
EXECUTABLE_NAME="DSHLauncher"
BUNDLE_ID="${BUNDLE_ID:-io.github.drswith.dsh-launcher}"
# Version label: a release tag passes it explicitly (0.2.0, 0.2.0-beta.1); other
# builds describe the nearest v* tag (0.2.0-3-gabc1234), or 0.1.0 before the first.
tag_version() {
  local described
  described="$(git -C "$ROOT" describe --tags --match 'v[0-9]*' 2>/dev/null)" || described=""
  echo "${described#v}"
}
APP_VERSION="${APP_VERSION:-$(tag_version)}"
APP_VERSION="${APP_VERSION:-0.1.0}"
[[ "$APP_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] \
  || die "APP_VERSION must look like 1.2.3 or 1.2.3-beta.1, got '$APP_VERSION'"
# CFBundleShortVersionString allows three integers only; the label keeps any suffix.
MARKETING_VERSION="${APP_VERSION%%-*}"
URL_SCHEME="${URL_SCHEME:-dsh-launcher}"
HOME_DIR_NAME="${HOME_DIR_NAME:-.dsh-launcher}"
DSH_PROFILE="${DSH_PROFILE:-launcher}"
DEFAULT_PORT="${DEFAULT_PORT:-31080}"
# Finder's Get Info copyright; matches LICENSE.
COPYRIGHT="${COPYRIGHT:-© 2026 Drswith}"

# --- Build identity ---
# CFBundleVersion must be 1-3 dot-separated integers that only ever increase;
# the default is the commit count (CI checks out the full history for it).
BUILD_NUMBER="${BUILD_NUMBER:-$(git -C "$ROOT" rev-list --count HEAD 2>/dev/null || echo 1)}"
[[ "$BUILD_NUMBER" =~ ^[0-9]+(\.[0-9]+){0,2}$ ]] \
  || die "BUILD_NUMBER must be 1-3 dot-separated integers (CFBundleVersion), got '$BUILD_NUMBER'"

# Commit of this repository, suffixed -dirty when tracked or untracked changes
# exist; shown in the About dialog and the launch log for traceability.
git_commit() {
  local hash
  hash="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null)" || { echo unknown; return; }
  if [ -n "$(git -C "$ROOT" status --porcelain --ignore-submodules=dirty 2>/dev/null)" ]; then
    hash="$hash-dirty"
  fi
  echo "$hash"
}
GIT_COMMIT="${GIT_COMMIT:-$(git_commit)}"
# Repository shown in the About dialog: origin as a browsable https URL, never with credentials.
repo_url() {
  local url
  url="$(git -C "$ROOT" remote get-url origin 2>/dev/null)" || url=""
  url="${url%.git}"
  case "$url" in
    git@*:*) url="${url#git@}"; url="https://${url/://}" ;;
    https://*) url="$(printf '%s' "$url" | sed -E 's#^https://[^/@]*@#https://#')" ;;
    *) url="" ;;
  esac
  echo "${url:-https://github.com/Drswith/dsh-launcher}"
}
REPO_URL="${REPO_URL:-$(repo_url)}"
# Build time as ISO 8601 with a colon offset (2026-09-19T10:05:10+08:00), like VS Code's Date.
BUILD_DATE="${BUILD_DATE:-$(date +%Y-%m-%dT%H:%M:%S%z | sed -E 's/([+-][0-9]{2})([0-9]{2})$/\1:\2/')}"

# --- Target architecture: the payload carries native modules, so one app per arch ---
ARCH="${ARCH:-$(uname -m)}"
case "$ARCH" in
  arm64) NODE_ARCH=arm64 ;;
  x86_64) NODE_ARCH=x64 ;;
  *) die "unsupported ARCH=$ARCH (use arm64 or x86_64)" ;;
esac

# --- Runtime versions: default to what the deepseek-harness submodule pins ---
submodule_dsh_version() {
  sed -n 's/^  "version": "\(.*\)",$/\1/p' "$HARNESS/apps/cli/package.json" 2>/dev/null | head -1
}
submodule_node_version() {
  sed -n "s/^const NODE_VERSION = '\(.*\)'$/\1/p" "$HARNESS/apps/desktop/scripts/prepare-runtime.ts" 2>/dev/null | head -1
}
submodule_pnpm_version() {
  sed -n 's/^  "packageManager": "pnpm@\([^"+]*\).*",$/\1/p' "$HARNESS/package.json" 2>/dev/null | head -1
}

DSH_VERSION="${DSH_VERSION:-$(submodule_dsh_version)}"
NODE_VERSION="${NODE_VERSION:-$(submodule_node_version)}"
NODE_VERSION="${NODE_VERSION:-24.17.0}"
PNPM_VERSION="${PNPM_VERSION:-$(submodule_pnpm_version)}"
PNPM_VERSION="${PNPM_VERSION:-11.7.0}"
# pnpm's per-request timeout. The default keeps a stalled request (its supply-chain
# policy check, say) from hanging the build; raise it on a slow link when a single
# package is too large to arrive in time, e.g. PNPM_FETCH_TIMEOUT=600000 make lock.
PNPM_FETCH_TIMEOUT="${PNPM_FETCH_TIMEOUT:-60000}"
NPM_REGISTRY="${NPM_REGISTRY:-https://registry.npmjs.org/}"
NPM_REGISTRY="${NPM_REGISTRY%/}/"
NODE_DIST_URL="${NODE_DIST_URL:-https://nodejs.org/download/release}"

# --- Runtime project: the committed package.json + pnpm-lock.yaml decide what is
# installed. DSH_VERSION above is only what the build expects (the submodule's
# version unless overridden), and the two must agree.
RUNTIME_PROJECT="${RUNTIME_PROJECT:-$ROOT/runtime}"
locked_dsh_version() {
  sed -n 's/^    "@deepseek-ai\/dsh": "\(.*\)"$/\1/p' "$RUNTIME_PROJECT/package.json" 2>/dev/null | head -1
}
LOCKED_DSH_VERSION="$(locked_dsh_version)"

# Refuse to build when the lock pins another dsh than the one expected.
check_locked_version() {
  [ -n "$DSH_VERSION" ] || die "DSH_VERSION is empty (init the deepseek-harness submodule or set DSH_VERSION)"
  [ -n "$LOCKED_DSH_VERSION" ] || die "$RUNTIME_PROJECT/package.json does not pin @deepseek-ai/dsh"
  [ "$LOCKED_DSH_VERSION" = "$DSH_VERSION" ] || die "runtime/package.json locks dsh $LOCKED_DSH_VERSION but the build expects $DSH_VERSION; run: make lock DSH_VERSION=$DSH_VERSION"
}

# --- Locations ---
BUILD_DIR="${BUILD_DIR:-$ROOT/build}"
CACHE_DIR="${CACHE_DIR:-$ROOT/.cache}"
PAYLOAD_DIR="$BUILD_DIR/payload/darwin-$ARCH"
APP_BUNDLE="$BUILD_DIR/$APP_NAME.app"

# --- Code signing (the app only; see build-app.sh) ---
# signing.local.env (git-ignored) names your certificate; see signing.local.env.example.
# Without it, builds are signed ad hoc ("-"), as CI and release builds are.
if [ -f "$ROOT/signing.local.env" ]; then
  # shellcheck source=/dev/null
  source "$ROOT/signing.local.env"
fi
CODESIGN_IDENTITY="${CODESIGN_IDENTITY:--}"
