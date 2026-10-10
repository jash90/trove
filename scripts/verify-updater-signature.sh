#!/usr/bin/env bash
# Verifies a Tauri updater signature (`<file>.sig`) against the updater public
# key compiled into the app (`plugins.updater.pubkey` in tauri.conf.json).
#
# Tauri's signatures are minisign signatures, base64-wrapped once more. This
# checks them with OpenSSL 3 alone, so no minisign install is needed:
#   1. the key id in the signature matches the key id of the public key,
#   2. the Ed25519 signature over BLAKE2b-512(file) holds ("ED", prehashed),
#      or over the file itself for legacy "Ed" signatures,
#   3. the global signature over (signature || trusted comment) holds.
#
# A release whose archive fails this would be refused by every installed copy,
# so release.sh runs it before anything is published.
set -euo pipefail

log() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<'EOF'
Usage: scripts/verify-updater-signature.sh <file> [<file>.sig] [pubkey]

  <file>      the signed artifact, e.g. Trove.app.tar.gz
  <file>.sig  its Tauri signature (default: <file>.sig)
  pubkey      base64 Tauri public key (default: plugins.updater.pubkey from
              src-tauri/tauri.conf.json)

Exits 0 when the signature is valid for that key, non-zero otherwise.
EOF
}

[[ $# -ge 1 && $1 != -h && $1 != --help ]] || { usage; [[ $# -ge 1 ]] && exit 0 || exit 2; }

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FILE="$1"
SIG="${2:-$1.sig}"
PUBKEY="${3:-$(jq -r '.plugins.updater.pubkey' "$ROOT/src-tauri/tauri.conf.json")}"

[[ -f $FILE ]] || die "no such file: $FILE"
[[ -f $SIG ]] || die "no such signature: $SIG"
[[ -n $PUBKEY && $PUBKEY != null ]] || die "no public key"

# OpenSSL 3 is required for Ed25519 with -rawin; LibreSSL in /usr/bin has none.
OPENSSL=""
for candidate in /opt/homebrew/opt/openssl@3/bin/openssl /usr/local/opt/openssl@3/bin/openssl "$(command -v openssl || true)"; do
  if [[ -x $candidate ]] && "$candidate" version 2>/dev/null | grep -q '^OpenSSL 3'; then
    OPENSSL="$candidate"
    break
  fi
done
[[ -n $OPENSSL ]] || die "OpenSSL 3 is required (brew install openssl@3)"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Both the key and the signature are a minisign file, base64-encoded as a whole.
printf '%s' "$PUBKEY" | base64 -d > "$WORK/key.txt" 2>/dev/null || die "public key is not base64"
base64 -d < "$SIG" > "$WORK/sig.txt" 2>/dev/null || die "signature is not base64"

sed -n 2p "$WORK/key.txt" | base64 -d > "$WORK/key.bin"
sed -n 2p "$WORK/sig.txt" | base64 -d > "$WORK/sig.bin"
TRUSTED_LINE="$(sed -n 3p "$WORK/sig.txt")"
sed -n 4p "$WORK/sig.txt" | base64 -d > "$WORK/global.bin"

[[ $(wc -c < "$WORK/key.bin") -eq 42 ]] || die "public key has an unexpected length"
[[ $(wc -c < "$WORK/sig.bin") -eq 74 ]] || die "signature has an unexpected length"
[[ $TRUSTED_LINE == "trusted comment: "* ]] || die "signature has no trusted comment"

hex() { xxd -p -c 256 | tr -d '\n'; }

KEY_ALG="$(head -c 2 "$WORK/key.bin")"
KEY_ID="$(dd if="$WORK/key.bin" bs=1 skip=2 count=8 2>/dev/null | hex)"
SIG_ALG="$(head -c 2 "$WORK/sig.bin")"
SIG_ID="$(dd if="$WORK/sig.bin" bs=1 skip=2 count=8 2>/dev/null | hex)"
[[ $KEY_ALG == Ed ]] || die "public key is not an Ed25519 minisign key"
[[ $KEY_ID == "$SIG_ID" ]] || die "signature was made with key $SIG_ID, expected $KEY_ID"

# SubjectPublicKeyInfo for a raw Ed25519 key: fixed 12-byte DER prefix + key.
{
  printf '302a300506032b6570032100'
  dd if="$WORK/key.bin" bs=1 skip=10 count=32 2>/dev/null | hex
} | xxd -r -p > "$WORK/key.der"
"$OPENSSL" pkey -pubin -inform DER -in "$WORK/key.der" -out "$WORK/key.pem" 2>/dev/null \
  || die "could not load the public key"

dd if="$WORK/sig.bin" bs=1 skip=10 count=64 2>/dev/null > "$WORK/ed.sig"

case "$SIG_ALG" in
  ED) "$OPENSSL" dgst -blake2b512 -binary "$FILE" > "$WORK/message" ;;
  Ed) cp "$FILE" "$WORK/message" ;;
  *) die "unknown signature algorithm: $SIG_ALG" ;;
esac

verify() { # <message> <signature>
  "$OPENSSL" pkeyutl -verify -pubin -inkey "$WORK/key.pem" -rawin \
    -in "$1" -sigfile "$2" > /dev/null 2>&1
}

verify "$WORK/message" "$WORK/ed.sig" || die "signature does not match $FILE"

# The global signature covers the file signature plus the trusted comment, so
# a swapped comment (it carries the file name and a timestamp) is caught too.
{ cat "$WORK/ed.sig"; printf '%s' "${TRUSTED_LINE#trusted comment: }"; } > "$WORK/global.msg"
verify "$WORK/global.msg" "$WORK/global.bin" || die "trusted comment signature is invalid"

log "updater signature OK: $(basename "$FILE") (key $KEY_ID, ${TRUSTED_LINE#trusted comment: })"
