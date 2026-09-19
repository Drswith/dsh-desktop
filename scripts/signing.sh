#!/usr/bin/env bash
# Code-signing helpers, sourced after config.sh. Modeled on deepseek-harness
# apps/desktop/scripts/verify-macos-signature.mjs and macos-runtime.ts: every
# Mach-O in the runtime is signed on its own with the hardened runtime, then
# each signature is checked for the expected signer, team, and runtime flag.

NODE_ENTITLEMENTS="$ROOT/Resources/Entitlements/node.entitlements"
MACH_O_MAGICS=" cafebabe cafebabf cefaedfe cffaedfe feedface feedfacf bebafeca bfbafeca "

signing_is_adhoc() {
  [ "$CODESIGN_IDENTITY" = "-" ]
}

# Human-readable signer, also recorded in the payload build identity.
signing_label() {
  if signing_is_adhoc; then echo "ad hoc"; else echo "$CODESIGN_IDENTITY ($CODESIGN_TEAM_ID)"; fi
}

# Fail early when the configured certificate is unusable.
check_signing_identity() {
  signing_is_adhoc && return 0
  [[ "$CODESIGN_TEAM_ID" =~ ^[A-Z0-9]{10}$ ]] \
    || die "CODESIGN_TEAM_ID must be the 10-character Team ID (the certificate's OU), got '$CODESIGN_TEAM_ID'"
  security find-identity -v -p codesigning | grep -qF "\"$CODESIGN_IDENTITY\"" \
    || die "no valid code-signing identity named '$CODESIGN_IDENTITY' in the keychain; see: security find-identity -v -p codesigning"
}

# Run codesign quietly; retry once because the timestamp service fails transiently.
run_codesign() {
  local output attempt
  for attempt in 1 2; do
    if output="$(/usr/bin/codesign "$@" 2>&1)"; then return 0; fi
    if [ "$attempt" = 1 ]; then sleep 2; fi
  done
  echo "$output" >&2
  return 1
}

is_macho() {
  local magic
  magic="$(od -An -tx1 -N4 "$1" 2>/dev/null | tr -d ' \n')"
  [ -n "$magic" ] && [[ "$MACH_O_MAGICS" == *" $magic "* ]]
}

# sign_code <path> <identifier> [entitlements]
sign_code() {
  local args=(--force --sign "$CODESIGN_IDENTITY" --identifier "$2" --options runtime)
  if signing_is_adhoc; then args+=(--timestamp=none); else args+=(--timestamp); fi
  if [ -n "${3:-}" ]; then args+=(--entitlements "$3"); fi
  run_codesign "${args[@]}" "$1" || die "codesign could not sign $1"
}

# Reject signatures that are not ours. `requires_runtime` is 0 for disk images.
# assert_signature <path> <requires_runtime>
assert_signature() {
  local path="$1" requires_runtime="$2" details
  details="$(/usr/bin/codesign --display --verbose=4 "$path" 2>&1)" || die "$path: cannot read its signature"
  if signing_is_adhoc; then
    grep -qx "Signature=adhoc" <<<"$details" || die "$path: expected an ad hoc signature"
  else
    grep -qxF "Authority=$CODESIGN_IDENTITY" <<<"$details" || die "$path: not signed by $CODESIGN_IDENTITY"
    grep -qxF "TeamIdentifier=$CODESIGN_TEAM_ID" <<<"$details" || die "$path: TeamIdentifier is not $CODESIGN_TEAM_ID"
    grep -q "^Timestamp=" <<<"$details" || die "$path: signature has no secure timestamp"
  fi
  if [ "$requires_runtime" = 1 ]; then
    grep -Eq 'flags=0x[0-9a-f]+\(([a-z-]+,)*runtime(,[a-z-]+)*\)' <<<"$details" \
      || die "$path: hardened runtime is not enabled"
  fi
}

verify_code() {
  run_codesign --verify --strict --verbose=2 "$1" || die "$1: signature verification failed"
  assert_signature "$1" 1
}

# Sign every Mach-O under a runtime tree; Node alone gets JIT entitlements.
sign_runtime_tree() {
  local root="$1" file relative identifier count=0
  while IFS= read -r -d '' file; do
    is_macho "$file" || continue
    relative="${file#"$root"/}"
    if [ "$relative" = "node/bin/node" ]; then
      sign_code "$file" "$BUNDLE_ID.runtime.node" "$NODE_ENTITLEMENTS"
    else
      identifier="$BUNDLE_ID.runtime.$(printf '%s' "$relative" | shasum -a 256 | cut -c1-64)"
      sign_code "$file" "$identifier"
    fi
    verify_code "$file"
    count=$((count + 1))
  done < <(find "$root" -type f -print0)
  [ "$count" -gt 0 ] || die "no Mach-O files found under $root"
  log "signed and verified $count runtime Mach-O files ($(signing_label))"
}

sign_app() {
  sign_code "$1" "$BUNDLE_ID"
  run_codesign --verify --deep --strict --verbose=2 "$1" || die "$1: signature verification failed"
  assert_signature "$1" 1
}

sign_disk_image() {
  signing_is_adhoc && return 0
  run_codesign --force --sign "$CODESIGN_IDENTITY" --timestamp "$1" || die "codesign could not sign $1"
  run_codesign --verify --strict --verbose=2 "$1" || die "$1: signature verification failed"
  assert_signature "$1" 0
}
