# Project Environment

- Inspected: 2026-08-22 (Europe/Warsaw)
- Current state: standalone Polish HTML architecture report plus Raycast and SuperCmd clipboard-history exports; no application implementation exists yet.
- Project type: static web document / future desktop application specification.
- React Native / Expo / native iOS / native Android: no.
- Recommended target described by the report: Tauri 2 shell, Rust core, React + TypeScript UI, SQLite WAL + FTS5, content-addressed blob storage.
- Existing runnable artifact: `raport-koncowy-clipboard-manager.html` (open directly or serve with `python3 -m http.server 8000`).
- Existing utility: `clipboard history/raycast-rayconfig-export-20260822-152334/decrypt-rayconfig.py` (Python 3; depends on `cryptography`).
- Toolchain available at inspection: Python 3.11.8, Node 22.19.0, npm 10.9.3, pnpm 10.33.0, Bun 1.3.11.
- Missing: `package.json`, lockfile, `Cargo.toml`, Tauri config, bundler config, tests, CI, README, and Git repository.
- Data caution: clipboard exports contain potentially sensitive personal data and must not be logged, published, or committed.
