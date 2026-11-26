# shellcheck shell=bash
# Sourced by bump-version.sh and release.sh: reads the version each manifest
# declares. Trove's version lives in three places and they must agree.

cargo_version() {
  awk '
    /^\[/ { section = $0 }
    section == "[workspace.package]" && /^version *=/ { gsub(/.*= *"|".*/, ""); print; exit }
  ' Cargo.toml
}

tauri_version() { jq -r '.version' src-tauri/tauri.conf.json; }
ui_version() { jq -r '.version' apps/desktop-ui/package.json; }

# check_versions [expected] — prints each file's version; fails on a mismatch.
check_versions() {
  local expected="${1:-$(cargo_version)}" ok=0 found
  for pair in "Cargo.toml:$(cargo_version)" \
              "src-tauri/tauri.conf.json:$(tauri_version)" \
              "apps/desktop-ui/package.json:$(ui_version)"; do
    found="${pair##*:}"
    if [[ $found == "$expected" ]]; then
      printf '   %-30s %s\n' "${pair%%:*}" "$found"
    else
      printf '   %-30s %s (expected %s)\n' "${pair%%:*}" "$found" "$expected" >&2
      ok=1
    fi
  done
  return $ok
}
