# Releasing Trove

Releases are built, signed, notarised and published from a Mac with one
command:

```bash
scripts/bump-version.sh 1.9.3   # commits "chore: release 1.9.3"
git push origin main
scripts/release.sh              # gates, build, sign, notarise, publish
```

The GitHub Actions workflows (`.github/workflows/ci.yml`, `release.yml`) do the
same work on a hosted macOS runner. They are kept as a fallback — see
[Manual GitHub Actions fallback](#manual-github-actions-fallback).

## Scripts

| Script | What it does |
|---|---|
| `scripts/ci.sh` | The CI gates, in CI order: `pnpm install --frozen-lockfile`, `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace`, `pnpm typecheck`, `pnpm test`, `pnpm build`, `pnpm tauri build --debug --no-bundle`. `--no-bundle` skips the last step. |
| `scripts/bump-version.sh X.Y.Z` | Writes the version into `Cargo.toml` (`[workspace.package]`), `src-tauri/tauri.conf.json` and `apps/desktop-ui/package.json`, refreshes `Cargo.lock`, checks all three agree and commits `chore: release X.Y.Z`. Does not tag or push. |
| `scripts/release.sh` | The release itself (below). |
| `scripts/verify-updater-signature.sh FILE` | Checks `FILE.sig` against the updater public key in `tauri.conf.json`, with OpenSSL 3 only. |
| `.githooks/pre-push` | Runs `scripts/ci.sh` before a push (tag-only pushes are skipped). Opt in once per clone: `git config core.hooksPath .githooks`. |

## `scripts/release.sh`

```
scripts/release.sh [--dry-run] [--draft] [--skip-ci]
```

- `--dry-run` builds, signs, notarises and staples for real and writes every
  artifact, but does not tag, push or touch GitHub. It only warns when you are
  not on `main` or the version is already tagged, so it can be run from any
  branch.
- `--draft` publishes the GitHub release as a draft. Installed copies ignore
  drafts, so nobody is offered the update until you publish it with
  `gh release edit vX.Y.Z --draft=false --latest`.
- `--skip-ci` skips `scripts/ci.sh` (e.g. right after a green run).

Steps, in order:

1. **Preflight** — Apple silicon Mac; required tools present; at least 15 GB
   free; clean working tree; on `main` and equal to `origin/main`; the three
   version files agree; the updater public key is not the placeholder;
   `release.yml` does not run on tag pushes (it would race this script); tag
   `vX.Y.Z` exists neither locally nor on `origin`; the signing identity is in
   the keychain; the notarytool profile works; the updater key and its
   password file exist; `gh` is logged in.
2. **Gates** — `scripts/ci.sh`.
3. **Build** — `pnpm tauri build --bundles app` with every `APPLE_*` and
   `TAURI_SIGNING_*` variable removed from its environment, so Tauri neither
   signs nor notarises on its own.
4. **Sign** — `codesign --force --deep --timestamp --options runtime` by the
   certificate's SHA-1, then a `TeamIdentifier` check and
   `codesign --verify --deep --strict`.
5. **Notarise the app** as a zip (`xcrun notarytool submit --keychain-profile
   … --wait`), staple it, `stapler validate`.
6. **Disk image** `Trove_X.Y.Z_aarch64.dmg` built with `hdiutil` from the
   stapled app plus an `/Applications` link; signed, notarised, stapled,
   validated.
7. **Gatekeeper** — `spctl -a -vv` on both the image and the app.
8. **Updater archive** `Trove.app.tar.gz` (`COPYFILE_DISABLE=1`, checked for
   AppleDouble files), signed with `pnpm tauri signer sign`, and the signature
   verified against the public key compiled into the app.
9. **Collect** everything in `dist/release/vX.Y.Z/` (gitignored): the `.app`,
   the `.dmg`, `Trove.app.tar.gz` + `.sig`, `latest.json`, `SHA256SUMS`.
10. **Publish** (not in `--dry-run`) — tag `vX.Y.Z` at `HEAD` and push the tag;
    create a draft release with the DMG and generated notes; write
    `latest.json` with those notes; upload the archive, its signature and
    `latest.json`; publish as the latest release (unless `--draft`).

Installed copies poll
`https://github.com/jash90/trove/releases/latest/download/latest.json`, so the
release only reaches them once it is published and marked latest. That is why
`latest.json` goes up last and the release stays a draft until it is there.

**Re-running.** If a run fails after the tag was pushed, fix the cause and run
`scripts/release.sh` again: when the tag points at `HEAD` and its release is
still a draft, the run resumes it and re-uploads the assets. A published
release is never overwritten.

## One-time machine setup

1. **Toolchain** — Xcode (with command line tools), Rust via `rustup` (the
   version in `rust-toolchain.toml` installs itself), Node, pnpm 10.33.0,
   `jq`, `gh` (`gh auth login`), OpenSSL 3 (`brew install openssl@3`, used to
   verify the updater signature).
2. **Signing certificate** — a *Developer ID Application* certificate with its
   private key in the login keychain. Find its SHA-1 with
   `security find-identity -v -p codesigning`. Two certificates on this account
   share a name, so it is always selected by SHA-1, never by name.
3. **Notarisation profile** — store an app-specific password for the Apple ID
   in the keychain once:

   ```bash
   xcrun notarytool store-credentials local-release \
     --apple-id <apple-id> --team-id H2X8YGN869
   ```

4. **`~/.config/local-release/apple.env`** (not in the repo):

   ```bash
   APPLE_ID=<apple-id>
   APPLE_TEAM_ID=H2X8YGN869
   APPLE_SIGNING_IDENTITY=<certificate SHA-1>
   NOTARY_PROFILE=local-release
   ```

5. **Updater key** — the minisign private key made by `tauri signer generate`,
   e.g. `~/.tauri/trove-updater.key`, with its password in
   `~/.tauri/trove-updater.key.password`, and
   **`~/.config/local-release/tauri-updater.env`**:

   ```bash
   TROVE_UPDATER_KEY=$HOME/.tauri/trove-updater.key
   ```

   Its public half is `plugins.updater.pubkey` in `src-tauri/tauri.conf.json`
   and is compiled into every copy of the app. **Losing the private key strands
   every installed copy on the version it has** — keep a backup.

6. Optional: `git config core.hooksPath .githooks` to run the gates before
   every push.

## Troubleshooting

- **`signing identity … is not in the keychain`** — check
  `security find-identity -v -p codesigning`; the SHA-1 in `apple.env` must be
  one of the valid identities listed.
- **`notarytool profile … does not work`** — re-run
  `xcrun notarytool store-credentials local-release …`; app-specific passwords
  are revoked when the Apple ID password changes.
- **Notarisation `Invalid`** — the script prints the notarytool log. The usual
  causes are a missing hardened runtime or an unsigned nested binary; the sign
  step uses `--deep --options runtime`, so check whether the build added a new
  executable outside `Contents/MacOS`.
- **`notarytool` hangs with no output** — a disk image left attached by an
  interrupted run. `hdiutil info`, then `hdiutil detach /dev/diskN -force`.
  The script detaches its own images on exit.
- **`the bundle is not signed by team …`** — signing failed but left an older
  signature in place. Usually an ambiguous identity; use the SHA-1.
- **`only N GB free`** — a release build needs about 15 GB; `cargo clean`
  in other worktrees frees the most.
- **`updater signature … does not match`** — the key in `tauri-updater.env`
  is not the one whose public half is in `tauri.conf.json`. Do not ship: every
  installed copy would refuse the update.
- **`HEAD is not origin/main`** — release from what is on GitHub: push (or
  pull) first. Use `--dry-run` to test a branch.

## Manual GitHub Actions fallback

When no signing Mac is available, the same release can be cut on a hosted
runner (macOS minutes are billed at 10×, so this is the exception):

1. Bump and push as above, then create and push the tag:
   `git tag vX.Y.Z && git push origin vX.Y.Z`.
2. Run **Actions → Release → Run workflow** with the tag, or
   `gh workflow run release.yml -f tag=vX.Y.Z`.

It needs these repository secrets: `MACOS_CERTIFICATE`,
`MACOS_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_API_KEY`,
`APPLE_API_KEY_ID`, `APPLE_API_ISSUER`, `TAURI_SIGNING_PRIVATE_KEY`,
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` (described at the top of `release.yml`).
The CI gates can likewise be run with **Actions → CI → Run workflow**.
