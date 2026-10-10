#!/usr/bin/env bash
# The gates the project is developed against, in the same order the GitHub
# Actions workflow (.github/workflows/ci.yml) ran them. Run it before every
# push; .githooks/pre-push does so when hooks are enabled:
#
#   git config core.hooksPath .githooks
set -euo pipefail

log() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<'EOF'
Usage: scripts/ci.sh [--no-bundle] [--no-install]

Runs, in order:
  pnpm install --frozen-lockfile
  cargo fmt --all --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  pnpm typecheck
  pnpm test            (vitest, single run)
  pnpm build
  pnpm tauri build --debug --no-bundle

Options:
  --no-bundle   skip the last (slowest) step, the debug application build
  --no-install  skip pnpm install (node_modules already up to date)
  -h, --help    show this help
EOF
}

BUNDLE=1
INSTALL=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-bundle) BUNDLE=0 ;;
    --no-install) INSTALL=0 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown option: $1" ;;
  esac
  shift
done

cd "$(dirname "${BASH_SOURCE[0]}")/.."

for tool in cargo pnpm; do
  command -v "$tool" > /dev/null || die "$tool is not installed"
done

export CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}"

step() {
  local name="$1"; shift
  log "$name"
  "$@" || die "$name failed"
}

[[ $INSTALL -eq 1 ]] && step "Dependencies" pnpm install --frozen-lockfile
step "Formatting" cargo fmt --all --check
step "Lints" cargo clippy --workspace --all-targets -- -D warnings
step "Rust tests" cargo test --workspace
step "Types" pnpm typecheck
# CI=1 makes vitest run once instead of starting its watcher.
step "Interface tests" env CI=1 pnpm test
step "Interface build" pnpm build
# Through pnpm, not `cargo tauri`: the CLI is a devDependency here.
# Last because it is the slowest, and because everything above has to hold
# before the bundle is worth producing.
if [[ $BUNDLE -eq 1 ]]; then
  step "Application bundle" pnpm tauri build --debug --no-bundle
fi

log "all gates passed"
