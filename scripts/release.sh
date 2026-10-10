#!/usr/bin/env bash
# Cuts a Trove release on this Mac: gates, build, sign, notarise, staple,
# disk image, updater archive, GitHub release.
#
# This is the GitHub Actions release workflow (.github/workflows/release.yml),
# step for step, run locally: the signing identity and the notarisation
# credentials stay in this machine's keychain instead of repository secrets.
# See RELEASING.md for the one-time setup.
set -euo pipefail

log() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<'EOF'
Usage: scripts/release.sh [--dry-run] [--draft] [--skip-ci]

Builds, signs, notarises and publishes the version the manifests declare
(Cargo.toml, src-tauri/tauri.conf.json, apps/desktop-ui/package.json) as
tag vX.Y.Z. Artifacts land in dist/release/vX.Y.Z/.

Options:
  --dry-run   do everything up to and including notarisation and the updater
              files, but do not tag, push or touch GitHub. Allowed off main and
              for a version that is already tagged (it only warns).
  --draft     publish the GitHub release as a draft (installed copies ignore
              drafts, so nobody is offered the update until you publish it)
  --skip-ci   do not run scripts/ci.sh first
  -h, --help  show this help

Re-running for a tag whose GitHub release is still a draft resumes it: the
tag is reused and the assets are re-uploaded.

Environment (read from ~/.config/local-release/*.env):
  APPLE_SIGNING_IDENTITY  SHA-1 of the Developer ID Application certificate
  APPLE_TEAM_ID           expected TeamIdentifier of the signature
  NOTARY_PROFILE          xcrun notarytool keychain profile
  TROVE_UPDATER_KEY       path to the updater's minisign private key; its
                          password is read from "$TROVE_UPDATER_KEY.password"
EOF
}

DRY_RUN=0
DRAFT=0
SKIP_CI=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run) DRY_RUN=1 ;;
    --draft) DRAFT=1 ;;
    --skip-ci) SKIP_CI=1 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown option: $1" ;;
  esac
  shift
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

REPO="jash90/trove"
CONFIG_DIR="${LOCAL_RELEASE_CONFIG:-$HOME/.config/local-release}"
MIN_FREE_GB=15

# --- configuration ----------------------------------------------------------

# Sourced without exporting: `pnpm tauri build` must not see APPLE_* (it would
# sign and try to notarise on its own, and its bundle fails strict
# verification) nor the updater key. Each step passes what it needs.
[[ -f $CONFIG_DIR/apple.env ]] || die "missing $CONFIG_DIR/apple.env (see RELEASING.md)"
[[ -f $CONFIG_DIR/tauri-updater.env ]] || die "missing $CONFIG_DIR/tauri-updater.env (see RELEASING.md)"
# shellcheck source=/dev/null
source "$CONFIG_DIR/apple.env"
# shellcheck source=/dev/null
source "$CONFIG_DIR/tauri-updater.env"

: "${APPLE_SIGNING_IDENTITY:?not set in apple.env}"
: "${APPLE_TEAM_ID:?not set in apple.env}"
: "${NOTARY_PROFILE:?not set in apple.env}"
: "${TROVE_UPDATER_KEY:?not set in tauri-updater.env}"
UPDATER_KEY="${TROVE_UPDATER_KEY/#\~/$HOME}"
UPDATER_KEY_PASSWORD_FILE="$UPDATER_KEY.password"

# shellcheck source=scripts/versions.sh
source "$ROOT/scripts/versions.sh"

VERSION="$(cargo_version)"
TAG="v$VERSION"
[[ $TAG =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "refusing a version that is not MAJOR.MINOR.PATCH: $VERSION"

OUT="$ROOT/dist/release/$TAG"
APP="$ROOT/target/release/bundle/macos/Trove.app"
DMG_NAME="Trove_${VERSION}_aarch64.dmg"
ARCHIVE_NAME="Trove.app.tar.gz"

# Pipelines below never end in `grep -q`: with pipefail, grep exiting at the
# first match kills the writer with SIGPIPE and the whole check reads as false.

# team_of <path> — the TeamIdentifier a signature carries.
team_of() { codesign -dvv "$1" 2>&1 | sed -n 's/^TeamIdentifier=//p'; }

# Strict in a real run, a warning in a dry run.
gate() {
  if [[ $DRY_RUN -eq 1 ]]; then warn "$* (ignored: --dry-run)"; else die "$*"; fi
}

# --- preflight --------------------------------------------------------------

log "Preflight for $TAG$([[ $DRY_RUN -eq 1 ]] && echo ' (dry run)')"

[[ $(uname -s) == Darwin && $(uname -m) == arm64 ]] || die "releases are built on an Apple silicon Mac"

for tool in cargo pnpm jq git xcrun codesign hdiutil spctl ditto tar security; do
  command -v "$tool" > /dev/null || die "$tool is not installed"
done
[[ $DRY_RUN -eq 1 ]] || command -v gh > /dev/null || die "gh is not installed"

FREE_GB=$(df -g "$ROOT" | awk 'NR==2 { print $4 }')
[[ $FREE_GB -ge $MIN_FREE_GB ]] || die "only ${FREE_GB} GB free; a release build needs at least ${MIN_FREE_GB} GB"

[[ -z $(git status --porcelain) ]] || die "the working tree is not clean"

git fetch --quiet --tags origin main
BRANCH="$(git rev-parse --abbrev-ref HEAD)"
HEAD_SHA="$(git rev-parse HEAD)"
[[ $BRANCH == main ]] || gate "not on main (on $BRANCH)"
[[ $HEAD_SHA == "$(git rev-parse origin/main)" ]] || gate "HEAD is not origin/main; push or pull first"

log "Version files"
check_versions "$VERSION" || die "the version files disagree"

# A build carrying the placeholder would check for updates and refuse every
# one it found, and the only way out of that is a manual install.
if grep -q "REPLACE_WITH_TROVE_UPDATER_PUBLIC_KEY" src-tauri/tauri.conf.json; then
  die "src-tauri/tauri.conf.json still has the placeholder updater public key"
fi

# Pushing the tag must not also start the hosted release workflow: the two
# would race to build and publish the same release.
if awk '/^on:/ { on = 1; next } /^[^ #]/ { on = 0 } on && /tags:/ { found = 1 } END { exit !found }' \
    .github/workflows/release.yml 2> /dev/null; then
  gate ".github/workflows/release.yml still runs on tag pushes; make it workflow_dispatch-only first"
fi

# The tag must be new — unless it points at HEAD and its release is still a
# draft, in which case this run resumes that release.
RESUME=0
LOCAL_TAG="$(git rev-parse -q --verify "refs/tags/$TAG^{commit}" || true)"
REMOTE_TAG="$(git ls-remote --tags origin "refs/tags/$TAG" | awk '{ print $1 }')"
if [[ -n $LOCAL_TAG || -n $REMOTE_TAG ]]; then
  if [[ $DRY_RUN -eq 1 ]]; then
    warn "$TAG already exists (ignored: --dry-run)"
  else
    [[ ${LOCAL_TAG:-$HEAD_SHA} == "$HEAD_SHA" ]] || die "$TAG exists locally and points elsewhere"
    IS_DRAFT="$(gh release view "$TAG" -R "$REPO" --json isDraft --jq .isDraft 2>/dev/null || echo missing)"
    [[ $IS_DRAFT == true ]] || die "$TAG already exists and its release is not a resumable draft ($IS_DRAFT)"
    git fetch --quiet origin "refs/tags/$TAG:refs/tags/$TAG"
    [[ $(git rev-parse "refs/tags/$TAG^{commit}") == "$HEAD_SHA" ]] || die "$TAG on origin points elsewhere"
    RESUME=1
    log "resuming the draft release $TAG"
  fi
fi

# The certificate is picked by SHA-1: two Developer ID certificates on this
# account share a name, and signing by name fails as ambiguous.
security find-identity -v -p codesigning | grep -F > /dev/null "$APPLE_SIGNING_IDENTITY" \
  || die "signing identity $APPLE_SIGNING_IDENTITY is not in the keychain"
xcrun notarytool history --keychain-profile "$NOTARY_PROFILE" > /dev/null 2>&1 \
  || die "notarytool profile '$NOTARY_PROFILE' does not work (see RELEASING.md)"
[[ -f $UPDATER_KEY ]] || die "updater private key not found: $UPDATER_KEY"
[[ -f $UPDATER_KEY_PASSWORD_FILE ]] || die "updater key password not found: $UPDATER_KEY_PASSWORD_FILE"
if [[ $DRY_RUN -eq 0 ]]; then
  gh auth status > /dev/null 2>&1 || die "gh is not logged in"
fi

# --- gates ------------------------------------------------------------------

if [[ $SKIP_CI -eq 1 ]]; then
  warn "skipping scripts/ci.sh (--skip-ci)"
else
  "$ROOT/scripts/ci.sh"
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/trove-release.XXXXXX")"
cleanup() {
  # A disk image left attached by an interrupted run deadlocks the next
  # notarytool submission, so anything mounted from here is detached.
  hdiutil info | awk -v w="$WORK" '
      /^image-path/ { hit = index($0, w) > 0 }
      hit && /^\/dev\/disk[0-9]+[ \t]/ { print $1; hit = 0 }' \
    | while read -r dev; do hdiutil detach "$dev" -force > /dev/null 2>&1 || true; done
  rm -rf "$WORK"
}
trap cleanup EXIT

# --- build ------------------------------------------------------------------

# Only the .app bundle: the disk image is built by hand below. Tauri's own dmg
# step drives Finder, cannot unmount when anything else holds a disk image, and
# would package an unsigned copy anyway.
log "Build"
env -u APPLE_SIGNING_IDENTITY -u APPLE_CERTIFICATE -u APPLE_ID -u APPLE_PASSWORD \
    -u APPLE_TEAM_ID -u APPLE_API_KEY -u APPLE_API_ISSUER \
    -u TAURI_SIGNING_PRIVATE_KEY -u TAURI_SIGNING_PRIVATE_KEY_PATH \
    -u TAURI_SIGNING_PRIVATE_KEY_PASSWORD \
    pnpm tauri build --bundles app
[[ -d $APP ]] || die "the build produced no $APP"

# --- sign -------------------------------------------------------------------

# After the build, never before: tauri regenerates the bundle and would undo
# it, and its own bundle fails `codesign --deep --strict` with "code has no
# resources but signature indicates they must be present".
log "Sign the application"
codesign --force --deep --timestamp --options runtime --sign "$APPLE_SIGNING_IDENTITY" "$APP"
# The exit code is not enough: a failed sign can leave the previous signature
# in place, so the identity itself is what gets checked.
[[ $(team_of "$APP") == "$APPLE_TEAM_ID" ]] \
  || die "the bundle is not signed by team $APPLE_TEAM_ID"
codesign --verify --deep --strict --verbose=2 "$APP"

# notarise <file> — submits and waits; fails unless Apple accepts it.
notarise() {
  local file="$1" result status id
  # --timeout so a stalled upload ends by itself.
  result="$(xcrun notarytool submit "$file" --keychain-profile "$NOTARY_PROFILE" \
    --wait --timeout 30m --output-format json)" || true
  status="$(jq -r '.status // "unknown"' <<< "$result" 2>/dev/null || echo unknown)"
  id="$(jq -r '.id // ""' <<< "$result" 2>/dev/null || true)"
  if [[ $status != Accepted ]]; then
    [[ -n $id ]] && xcrun notarytool log "$id" --keychain-profile "$NOTARY_PROFILE" >&2 || true
    die "notarisation of $(basename "$file") ended as: $status"
  fi
  log "notarised $(basename "$file") ($id)"
}

# The application is notarised as a zip rather than inside the image:
# notarytool mounts a .dmg to inspect it, and a mount left behind by an
# interrupted run deadlocks every attempt after it. A zip has nothing to mount.
log "Notarise and staple the application"
ditto -c -k --keepParent "$APP" "$WORK/Trove.zip"
notarise "$WORK/Trove.zip"
xcrun stapler staple "$APP"
xcrun stapler validate "$APP"

# --- disk image -------------------------------------------------------------

# From the stapled application, so the copy someone drags to /Applications
# carries its ticket and opens without an online Gatekeeper check.
log "Build the disk image"
DMG="$WORK/$DMG_NAME"
mkdir -p "$WORK/dmg-stage"
ditto "$APP" "$WORK/dmg-stage/Trove.app"
ln -s /Applications "$WORK/dmg-stage/Applications"
hdiutil create -volname "Trove" -srcfolder "$WORK/dmg-stage" -ov -format UDZO "$DMG" > /dev/null

log "Sign, notarise and staple the disk image"
codesign --force --timestamp --sign "$APPLE_SIGNING_IDENTITY" "$DMG"
[[ $(team_of "$DMG") == "$APPLE_TEAM_ID" ]] \
  || die "the disk image is not signed by team $APPLE_TEAM_ID"
notarise "$DMG"
xcrun stapler staple "$DMG"
xcrun stapler validate "$DMG"

# The one check that speaks for the person downloading this: both the image
# and the application inside it must come back as notarised.
log "Verify Gatekeeper accepts both"
spctl -a -vv -t open --context context:primary-signature "$DMG"
spctl -a -vv "$APP"

# --- updater ----------------------------------------------------------------

# What an installed copy downloads to update itself. Packed from the stapled
# application — not by `tauri build`, whose archive would predate the
# signature — so the bundle that replaces the old one passes Gatekeeper as is.
#
# COPYFILE_DISABLE keeps macOS tar from adding AppleDouble `._` files, which
# would land inside the bundle and break its sealed resources.
log "Build and sign the updater archive"
ARCHIVE="$WORK/$ARCHIVE_NAME"
COPYFILE_DISABLE=1 tar -czf "$ARCHIVE" -C "$(dirname "$APP")" Trove.app
tar -tzf "$ARCHIVE" | grep > /dev/null "^Trove.app/Contents/MacOS/" \
  || die "the updater archive does not hold the application"
if tar -tzf "$ARCHIVE" | grep > /dev/null "/\._"; then
  die "the updater archive carries AppleDouble files"
fi
TAURI_SIGNING_PRIVATE_KEY_PATH="$UPDATER_KEY" \
TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$(cat "$UPDATER_KEY_PASSWORD_FILE")" \
  pnpm --silent tauri signer sign "$ARCHIVE" > /dev/null
[[ -s $ARCHIVE.sig ]] || die "signing the updater archive produced no signature"
# Against the public key compiled into the app: a mismatch here would be
# refused by every installed copy.
"$ROOT/scripts/verify-updater-signature.sh" "$ARCHIVE"

# --- collect ----------------------------------------------------------------

log "Collect artifacts in ${OUT#"$ROOT/"}"
rm -rf "$OUT"
mkdir -p "$OUT"
cp "$DMG" "$ARCHIVE" "$ARCHIVE.sig" "$OUT/"
ditto "$APP" "$OUT/Trove.app"

# write_manifest <notes> — latest.json, what installed copies poll through
# releases/latest/download/latest.json.
write_manifest() {
  jq -n \
    --arg version "$VERSION" \
    --arg notes "$1" \
    --arg date "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg signature "$(cat "$OUT/$ARCHIVE_NAME.sig")" \
    --arg url "https://github.com/$REPO/releases/download/$TAG/$ARCHIVE_NAME" \
    '{version: $version, notes: $notes, pub_date: $date,
      platforms: {"darwin-aarch64": {signature: $signature, url: $url}}}' \
    > "$OUT/latest.json"
  jq . "$OUT/latest.json"
}

if [[ $DRY_RUN -eq 1 ]]; then
  # The notes GitHub would generate, without creating anything.
  NOTES="$(gh api "repos/$REPO/releases/generate-notes" -f tag_name="$TAG" \
    -f target_commitish="$HEAD_SHA" --jq .body 2>/dev/null || echo "Trove $VERSION")"
  write_manifest "$NOTES"
  (cd "$OUT" && shasum -a 256 "$DMG_NAME" "$ARCHIVE_NAME" > SHA256SUMS)
  log "Dry run complete: nothing was tagged, pushed or published."
  ls -l "$OUT"
  exit 0
fi

# --- publish ----------------------------------------------------------------

(cd "$OUT" && shasum -a 256 "$DMG_NAME" "$ARCHIVE_NAME" > SHA256SUMS)

if [[ $RESUME -eq 0 ]]; then
  log "Tag $TAG"
  git tag "$TAG" "$HEAD_SHA"
  git push origin "refs/tags/$TAG"
fi

if gh release view "$TAG" -R "$REPO" > /dev/null 2>&1; then
  gh release upload "$TAG" -R "$REPO" --clobber "$OUT/$DMG_NAME"
else
  # A draft until the updater files are on it: published now, it would be
  # `releases/latest` without a `latest.json`, and every check made in that
  # minute would report the server as unreachable.
  gh release create "$TAG" "$OUT/$DMG_NAME" -R "$REPO" --verify-tag \
    --title "$TAG" --generate-notes --draft
fi

# The notes are the ones --generate-notes just wrote for the release.
log "Write the update manifest"
write_manifest "$(gh release view "$TAG" -R "$REPO" --json body --jq .body)"

# Last, because this is the step that makes the release reach people.
log "Upload the updater files"
gh release upload "$TAG" -R "$REPO" --clobber \
  "$OUT/$ARCHIVE_NAME" "$OUT/$ARCHIVE_NAME.sig" "$OUT/latest.json"

if [[ $DRAFT -eq 1 ]]; then
  log "Left $TAG as a draft (--draft). Publish it with:"
  echo "   gh release edit $TAG -R $REPO --draft=false --latest"
else
  gh release edit "$TAG" -R "$REPO" --draft=false --latest
  log "Published $TAG: https://github.com/$REPO/releases/tag/$TAG"
fi
