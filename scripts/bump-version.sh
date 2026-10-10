#!/usr/bin/env bash
# Writes a new version into every file that declares one, checks they agree,
# refreshes Cargo.lock and commits `chore: release X.Y.Z`. Does not tag or
# push: scripts/release.sh does that once the build has held.
set -euo pipefail

log() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<'EOF'
Usage: scripts/bump-version.sh X.Y.Z

Sets the version in:
  Cargo.toml                    [workspace.package] version
  src-tauri/tauri.conf.json     version
  apps/desktop-ui/package.json  version
then updates Cargo.lock and commits "chore: release X.Y.Z".
The working tree must be clean. Nothing is tagged or pushed.
EOF
}

[[ $# -eq 1 ]] || { usage >&2; exit 2; }
[[ $1 == -h || $1 == --help ]] && { usage; exit 0; }

VERSION="$1"
[[ $VERSION =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "not a MAJOR.MINOR.PATCH version: $VERSION"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

for tool in jq cargo git; do
  command -v "$tool" > /dev/null || die "$tool is not installed"
done

[[ -z $(git status --porcelain) ]] || die "the working tree is not clean"

# shellcheck source=scripts/versions.sh
source "$ROOT/scripts/versions.sh"

CURRENT="$(cargo_version)"
[[ $CURRENT != "$VERSION" ]] || die "already at $VERSION"
log "bumping $CURRENT -> $VERSION"

# Only the version line inside [workspace.package]: dependency pins elsewhere
# in the file also start with `version`.
awk -v v="$VERSION" '
  /^\[/ { section = $0 }
  section == "[workspace.package]" && /^version *=/ && !done { print "version = \"" v "\""; done = 1; next }
  { print }
' Cargo.toml > Cargo.toml.tmp && mv Cargo.toml.tmp Cargo.toml

for json in src-tauri/tauri.conf.json apps/desktop-ui/package.json; do
  # A targeted substitution rather than `jq . > file`, which would reformat
  # the whole file. Only the first top-level "version" key is touched.
  awk -v v="$VERSION" '
    !done && /^  "version": "[^"]*"/ { sub(/"version": "[^"]*"/, "\"version\": \"" v "\""); done = 1 }
    { print }
  ' "$json" > "$json.tmp" && mv "$json.tmp" "$json"
done

# The workspace crates inherit the version, and Cargo.lock records it.
cargo update --workspace --offline --quiet

check_versions "$VERSION" || die "version files disagree after the bump"

git add Cargo.toml Cargo.lock src-tauri/tauri.conf.json apps/desktop-ui/package.json
git commit --quiet -m "chore: release $VERSION"
log "committed: $(git log -1 --format='%h %s')"
log "next: push to main, then run scripts/release.sh"
