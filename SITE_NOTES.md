# Site notes — analysis behind the GitHub Pages site

Source material for `docs/` (the site) and the PR summary. Everything below was
read from the repository on branch `feature/github-pages` (base: `main` @ `ebf90a8`,
version 1.8.0).

## What the application is

**Trove** — a local clipboard history manager for macOS, with a keyboard-driven
palette (`⌘Space`) that also launches applications, snaps windows, copies keys
from a personal key vault and opens a chat window with an LLM.

- Problem it solves: what you copy disappears after the next copy; existing
  managers (Raycast, SuperCmd) keep history in their own formats and/or cloud.
  Trove keeps unlimited history locally (SQLite WAL + FTS5 + content-addressed
  blobs), imports Raycast/SuperCmd archives, and makes no network requests by
  default.
- Project type: **desktop application** (Tauri 2 shell, Rust core, React UI).
- Platforms: **macOS verified** (`aarch64-apple-darwin`); Windows and Linux
  adapters exist but are explicitly "implemented, unverified" (README). The
  site presents it as a macOS application.
- Distribution: signed + notarised DMG for Apple Silicon on GitHub Releases
  (`Trove_1.8.0_aarch64.dmg`, via `.github/workflows/release.yml`).

## Audience

Primary: macOS power users who copy a lot (developers, designers, writers) and
care that their clipboard never leaves the machine. Secondary: developers who
want to build it or contribute (the repo documents quality gates, a benchmark
tool, an import CLI).

→ Tone: benefits first, concrete and honest (the README itself is very plain
about limits — the site keeps that voice). A separate "For developers" section
carries the stack and local run instructions.

## Stack and running locally

- Rust 1.96 (pinned in `rust-toolchain.toml`), Cargo workspace of 15 crates
  (`trove-core`, `-store`, `-search`, `-images`, `-import`, `-keyvault`,
  `-launcher`, `-link-preview`, `platform-*`, tools).
- Tauri 2.11, React 19, TypeScript 7, Vite 8, Vitest; pnpm 10.33, Node 22.
- `pnpm install` → `pnpm dev` (browser preview on synthetic data via
  `mockGateway`) / `pnpm tauri dev` (native app). `TROVE_DATA_DIR=data/dev`
  for a development database.
- No environment variables required. Optional: `TROVE_DATA_DIR`,
  `KEYVAULT_AGENT_FILE`. No external services required; the database is local
  SQLite. Optional network features: link previews, chat (Z.ai, OpenAI,
  OpenRouter, Anthropic — user's own API key), keyvault.

## Features (verified in code/README)

1. Clipboard capture with refusals: `ConcealedType`/`TransientType` (password
   managers) and a per-app exclusion list (Settings → Apps).
2. Palette with categories: Applications `1`, Clipboard history `2`, Key vault
   `3`, Windows `4`, Chat `5` (`src/components/PaletteHeader.tsx`,
   `CategoryTiles.tsx`); arrows + Enter on the tiles (1.7).
3. History: kinds text, link, image, file, colour, code, HTML; grouped duplicates
   with `×N` and up to five capture times; pinning; paste into the previous
   window (`Enter`, `⌘⇧V` plain text) — needs the Accessibility permission.
4. Search: FTS5 with Polish normalisation — diacritics and `ł` folded
   (`crates/trove-core/src/normalize.rs`, `unicode61 remove_diacritics 2`).
5. Link previews: title, icon, og:image — opt-in switch, strict SSRF guards
   (ports 80/443 only, private ranges refused after every redirect, head only).
6. Application launcher: scans standard folders lazily, fuzzy matching
   (prefix, word start, initials "vsc", ordered characters "chrm", bundle id),
   icons through NSWorkspace.
7. Windows category / global chords: Rectangle-style snapping (halves, quarters,
   maximise, centre) of the window the user was last in (1.8).
8. Chat window: Z.ai, OpenAI, OpenRouter, Anthropic; streamed answers with
   reasoning, Markdown, code artifacts, attachments; conversation in memory only.
9. Key vault: lists key names from a paired personal vault, copies a value
   straight to the clipboard without recording it in the history.
10. Import: Raycast `.rayconfig` (AES-256-CBC, password over terminal/stdin only),
    JSON, CSV; SuperCmd; idempotent; wizard in the app (`⌘I`) + CLI.
11. Retention: unlimited by default; optional N days; pinned entries survive;
    cleanup every 15 minutes in small batches.
12. Export history (Settings → Export), storage stats (Settings → Storage).
13. Menu-bar app; no Dock tile unless enabled (Settings → Shortcut, 1.6);
    Spotlight's `⌘Space` can be released from settings.
14. Performance (README, measured 23 Aug 2026, 1M records): selective search p95
    3.2 ms, first page 2.8 ms, RSS 131 MB (debug). Known limit: bulk terms ~811 ms.

Not mentioned on purpose: TypeSafe/privacy scan (removed in 1.6.0), any sync.

## i18n

The interface is English-only (strings hard-coded in components, no i18n
library). A few labels are in Polish inside the app (e.g. "Rozmiar", "Import
lokalny", "Bez limitu retencji") — left as they are. **One screenshot set** is
used in both language versions of the site.

## Visual identity

- Icon: `assets/brand/trove-icon-1024.png` (treasure chest, amber glow on dark).
- Tokens (`apps/desktop-ui/src/styles/tokens.css`): paper `#f4f2ec`, raised
  `#fffefa`, ink `#171d26`, muted `#69717d`, line `#dedbd1`, accent `#234e9d`,
  pin/amber `#b45309`. Light only in the app.
- Fonts (bundled via Fontsource, OFL): Space Grotesk (UI), Source Serif 4
  (body), JetBrains Mono (code). The site self-hosts the same woff2 files —
  no font CDN, in keeping with the product's "no network" stance.

## Repository facts

- Remote: `github.com/jash90/trove` → Pages URL `https://jash90.github.io/trove/`.
  No `CNAME`, no `homepage`.
- **Private repository** — GitHub Pages for private repos needs a paid plan
  (Pro/Team/Enterprise); the Releases link works only for people with access.
- License: MIT (`LICENSE`, © 2026 Bartlomiej Zimny). No contact address in the repo → footer links to GitHub issues.
- `/docs` held only `docs/superpowers/` (git-ignored notes) → site lives in
  `/docs`. No gh-pages branch, no Pages workflow before this change.
- CTA: `https://github.com/jash90/trove/releases/latest` (DMG download).

## Screenshots

`pnpm screenshots` (`scripts/screenshots/capture.mjs`) runs the real interface
in Chromium with `scripts/screenshots/fake-tauri.js` injected: the interface
sees `window.__TAURI_INTERNALS__` and uses its real Tauri gateway, answered by
an in-page fake core with fictional data. No application code changed.

- Windows at their real sizes from `tauri.conf.json` (palette 1040×680,
  settings 840×820, chat 560×720), 2× DPR, WebP q82 (34–95 KB each).
- No 1440×900 or 390×844 shots: a desktop app with fixed windows
  (`minWidth: 760`) has neither a browser viewport nor a mobile layout.
- Application icons in the launcher shot are generated tiles (letter on colour),
  not the real bundle icons — those would be Apple/vendor artwork.

## Verification

`node scripts/screenshots/check-site.mjs <url>` against `docs/` served under
`/trove/` (as Pages serves it): PL and EN × 1440×900 and 390×844 × light and
dark. It checks `lang`, hreflang, the language switch target, every image
loads, every image has `alt`, internal links and anchors resolve, no
horizontal overflow, no console errors, the lightbox (open, arrows, Escape,
focus return), the theme toggle and its persistence, and axe-core WCAG 2.1
A/AA. Last run: all checks passed.
