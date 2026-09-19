#!/usr/bin/env bash
# Boot a prepared runtime once with a throwaway DSH_HOME and wait for the
# `dsh web:` readiness line, exactly as the menu bar supervisor does.
# Usage: scripts/smoke-runtime.sh <runtime-dir>
source "$(dirname "$0")/config.sh"

RUNTIME="${1:?usage: smoke-runtime.sh <runtime-dir>}"
SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/dsh-launcher-smoke.XXXXXX")"
PORT="$("$RUNTIME/node/bin/node" -e 'const s=require("net").createServer();s.listen(0,"127.0.0.1",()=>{console.log(s.address().port);s.close()})')"
OUT="$SCRATCH/stdout.log"
ERR="$SCRATCH/stderr.log"
pid=""

cleanup() {
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    kill -TERM "$pid" 2>/dev/null || true
    for _ in $(seq 1 80); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
    kill -KILL "$pid" 2>/dev/null || true
  fi
  rm -rf "$SCRATCH"
}
trap cleanup EXIT

log "smoke: booting dsh on 127.0.0.1:$PORT with a scratch DSH_HOME"
(
  cd "$SCRATCH"
  env -i HOME="$HOME" PATH="/usr/bin:/bin:/usr/sbin:/sbin" LANG="${LANG:-en_US.UTF-8}" DSH_HOME="$SCRATCH/home" DSH_TELEMETRY_DISABLED=1 \
    "$RUNTIME/node/bin/node" "$RUNTIME/app/node_modules/@deepseek-ai/dsh/lib/bin.js" \
      --profile "$DSH_PROFILE" --from-default-profile web --no-open --host 127.0.0.1 --port "$PORT" \
      >"$OUT" 2>"$ERR" &
  echo $! > "$SCRATCH/pid"
)
pid="$(cat "$SCRATCH/pid")"

for _ in $(seq 1 240); do
  if grep -q '^dsh web: http://127.0.0.1:' "$OUT" 2>/dev/null; then
    status="$(curl -s --noproxy '*' -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/")"
    [ "$status" = "401" ] || die "smoke: expected 401 from an unauthenticated /, got $status"
    log "smoke: ready line printed and the server answers (HTTP $status)"
    exit 0
  fi
  if ! kill -0 "$pid" 2>/dev/null; then
    sed -E 's/token=[A-Za-z0-9_-]+/token=<redacted>/' "$OUT" "$ERR" >&2 || true
    die "smoke: dsh exited before becoming ready"
  fi
  sleep 0.5
done
sed -E 's/token=[A-Za-z0-9_-]+/token=<redacted>/' "$OUT" "$ERR" >&2 || true
die "smoke: dsh was not ready within 120s"
