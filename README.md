# Clipboard History

A local clipboard history manager. Rust core, Tauri 2 shell, React interface.
The history, its index and its blobs never leave this device, and fonts and
every other asset are bundled locally.

**One deliberate exception:** the preview of a link entry fetches the page's
title and icon. It can be turned off in settings — see "Link previews" below.
Apart from that, the application makes no network requests at all.

## Status

| Area | Status |
|---|---|
| Domain model, SQLite WAL + FTS5, CAS, search | done |
| Raycast and SuperCmd importers, verification CLI | done |
| React palette, previews, actions, import wizard, settings | done |
| Global shortcut, clipboard capture, tray, pasting | done |
| Retention and blob reclamation | done |
| Windows and Linux adapters | implemented, **unverified** |

## Support matrix

| Platform | Status | What that means |
|---|---|---|
| macOS | **verified** | Run and checked on `aarch64-apple-darwin`: history, import, capture, shortcut, tray. |
| Windows | **implemented** | The exclusion-policy code and the capability declaration exist and have tests that run on the host. Never compiled or run on Windows. |
| Linux | **implemented** | X11/Wayland session detection and the capability declaration have tests that run on the host. Never compiled or run on Linux. |

"Implemented" does not mean "works". Only the `aarch64-apple-darwin` target is
installed on this machine, so not even a cross-compile has been done:

```bash
rustup target add x86_64-pc-windows-msvc x86_64-unknown-linux-gnu
cargo check -p platform-windows --target x86_64-pc-windows-msvc
cargo check -p platform-linux --target x86_64-unknown-linux-gnu
```

Until those commands pass on a machine with the right targets, both adapters
stay unverified. Wayland without the data-control protocol does not allow
reading the clipboard in the background at all — the application then reports an
explicit `wayland_data_control_unavailable` state rather than pretending to a
history it cannot build.

## Requirements

- Rust 1.96 (pinned in `rust-toolchain.toml`)
- Node 22 and pnpm 10.33
- macOS with the Xcode command line tools (for the Tauri shell)

## Running it

```bash
pnpm install

# preview in an ordinary browser, on synthetic data, without Tauri
pnpm dev

# the native application
pnpm tauri dev
```

`pnpm dev` uses `mockGateway` — every visible entry is invented and comes from
no clipboard history at all.

To run the native application against your own development database:

```bash
CLIPBOARD_HISTORY_DATA_DIR="data/dev" pnpm tauri dev
```

Without that variable the application uses the operating system's data
directory.

## Global shortcut

`⌘⇧Space` (`Ctrl+Shift+Space` elsewhere) summons the palette and focuses it;
pressing it again hides it. Closing the window also only hides it — the
application exits solely through "Quit" in the menu bar item, because a
clipboard manager that stops running when its window closes quietly loses
history.

The menu bar icon shows the history on a left click; a right click opens a menu
with pausing capture, settings and quitting. The "Pause capture" entry doubles
as the indicator: if that is what it says, the application is recording.

If the shortcut is already taken by another application, registration fails and
the palette still works from its own window. Changing the shortcut in settings
is saved, but takes effect after a restart.

## Quality gates

All of these must pass before a task is closed:

```bash
cargo fmt --all --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings

pnpm --dir apps/desktop-ui test -- --run
pnpm --dir apps/desktop-ui typecheck
pnpm --dir apps/desktop-ui build

cargo tauri build --debug --no-bundle
```

Checking that the built bundle references no remote resource at all (only XML
namespaces are allowed, and those are never fetched):

```bash
grep -rEoh 'https?://[^"'"'"' )]*' apps/desktop-ui/dist \
  --include='*.js' --include='*.css' --include='*.html' \
  | grep -v 'www\.w3\.org' | sort -u
```

Installer signing is out of scope for the first release — which is why the build
stops at `--no-bundle`.

## Importing an archive

Importing happens entirely locally. Raycast and SuperCmd exports are supported:
the encrypted `.rayconfig` — the file Raycast actually writes — as well as JSON
(the source of truth) and CSV (the fallback format).

```bash
cargo run -p clipboard-import-cli -- analyze --source <directory-or-file>
cargo run -p clipboard-import-cli -- import  --source <directory-or-file> --data-dir data/dev
cargo run -p clipboard-import-cli -- verify  --data-dir data/dev --expect-records <n>
```

### Encrypted exports

A `.rayconfig` is `IV ‖ AES-256-CBC-PKCS7(gzip(JSON))` keyed on
`SHA-256(password)`. The CLI asks for the password on the terminal without
echoing it; in a script it is supplied over `--password-stdin`:

```bash
echo "$RAYCAST_PASSWORD" | cargo run -p clipboard-import-cli -- import \
  --source "Raycast 2026-08-22 14.39.05.rayconfig" --data-dir data/dev --password-stdin
```

**There is no flag that takes the password as an argument** — process arguments
are visible to anyone through `ps`, and the shell history records them. The
password is never stored or logged, and the decrypted content never reaches
disk: it is streamed straight into the parser. It is needed exactly once, during
analysis, because everything after that works on parsed records.

When a directory holds both a plain manifest and a `.rayconfig`, the plain one
wins — so that no password is asked for when none is needed.

`import` writes only to the data directory it was explicitly given, and `verify`
opens the database read-only. Both print counters and status codes only — never
content, paths or queries.

Repeating the same import creates no duplicates: every source record has a
stable fingerprint, and a second run reports it as `alreadyPresent`.

### What the import will not do

- **It does not read bytes from outside the chosen export directory.** An export
  file is untrusted input. An entry pointing at a file somewhere else keeps its
  path as metadata — the interface will show the location and let you open it —
  but the content does not enter the store.
- **It does not guess relationships the data does not carry.** A SuperCmd export
  has no field tying an image record to a file. Matching by file order is
  forbidden, so such records are skipped.
- **It does not keep entries that lead nowhere.** A file or image record enters
  the history only if its source still exists. The rest are accounted for as
  skipped: nothing happened to them, there is simply nothing to show or open.
  `imported + already present + skipped + failed` always equals the number of
  source records.
- **It does not reject records on a secret-detection heuristic.** An archive may
  hold sensitive data; the import wizard warns about that before it starts.
- **It cannot tell you the password was correct.** AES-CBC carries no
  authentication tag, so the importer rejects only what is plainly wrong: the
  PKCS7 padding and the gzip signature. Those two checks together are the whole
  of its wrong-password detection.

## Capture and pasting

The application records what you copy until capture is paused. It refuses —
before reading anything — entries the source application marked `ConcealedType`
or `TransientType`; that is how password managers mark theirs. It also refuses
anything from an application on the exclusion list.

`Enter` and `⌘⇧V` ask for a paste into the window you were in before the palette
opened. That needs the Accessibility permission (System Settings → Privacy &
Security → Accessibility). Without it the entry still reaches the clipboard, and
the application says why it did not paste instead of staying silent.

## Retention and disk space

History is unlimited by default. Turning retention on in settings permanently
deletes entries older than the given number of days — except pinned ones,
because pinning is an explicit "keep this". Cleanup runs in small batches every
quarter of an hour, so it never blocks recording.

Blobs nothing refers to any more are released in the same pass. The scan only
observes; whether a blob really is unused is decided by the writer immediately
before deletion, because between the scan and the deletion an import may have
started using it.

## Performance on a large history

History is unlimited, so "does this still work at a million entries" is a gate,
not a curiosity. `tools/clipboard-bench` builds a synthetic history from a seed
— it reads no real data — and measures what a user actually feels.

```bash
cargo run --release -p clipboard-bench -- generate --data-dir data/bench-large --records 1000000 --seed 42
cargo run --release -p clipboard-bench -- measure  --data-dir data/bench-large --queries 200
```

Measured 23 August 2026, macOS on `aarch64-apple-darwin`, one million records,
a 772 MB database plus 307 MB of blobs:

| Measurement | Result | Budget |
|---|---|---|
| Selective search, p95 | 3.2 ms | 50 ms |
| Selective search with an open reader, p95 | 0.9 ms | — |
| First page of the list (shortcut to results) | 2.8 ms | 100 ms |
| Scrolling: first page → 400th page | 2.6 ms → 3.4 ms | no growth |
| One bounded blob-reclamation pass | 100–125 ms | bounded |
| Application RSS with a million-record database | 131 MB (debug build) | 150 MB |
| Write throughput while generating | about 499 records/s | — |

Scrolling does not slow down with depth, because pagination follows the
`(captured_at_ms, event_id)` key rather than a growing `OFFSET`.

**A known limit.** A query for a word much of the history contains costs
hundreds of milliseconds — 811 ms p95 for a term matching 600 thousand records.
Relevance cannot be established without scoring every match. In the synthetic
set every record is built from a twelve-word vocabulary, so even the rarest word
hits 8% of the database; a real history has a long tail of words and produces
this case hardly ever. The 50 ms budget is met for selective queries and is
**not** met for bulk terms.

## Link previews

A link entry shows the domain, the path and — when fetching is enabled — the
page's title and icon. **This is the only place the application talks to the
network.**

What that costs, said plainly: with fetching on, opening the palette queries the
pages visible in the list, so each of those domains learns that you are looking
at your clipboard right now — along with your IP address. The result is
remembered permanently, failures included, so the same page is asked once.

The boundaries that always hold:

- `http` and `https` only; no other scheme is fetched,
- **ports 80 and 443 only** — otherwise a clipboard entry would be a way to
  knock on every service this machine can see,
- **never** local and private addresses (`localhost`, `0.0.0.0/8`,
  `127.0.0.0/8`, `10/8`, `172.16/12`, `192.168/16`, `169.254/16`, `.local`, IPv6
  ULA, and IPv4 addresses smuggled inside IPv6 through 6to4, Teredo and NAT64) —
  checked both in the name and in the address DNS returns, **after every
  redirect**, so that a preview does not become a scanner of your network,
- the checked address is **pinned to the connection**, so a name cannot answer
  with something else between the check and the connection,
- hard limits on time, response size and redirect count; no cookies and no
  JavaScript execution,
- only `<title>`, the icon reference and `og:image` (the picture a page
  nominates for itself) are read; nothing else is parsed or stored,
- a link leading straight to an image is recognised by its response type and
  becomes the thumbnail itself,
- a page's picture is downscaled before it is stored, so a history of links does
  not turn into a picture archive,
- only the `<head>` section is read — up to `</head>`, no further,
- the icon reaches the window as bytes from the local store — **the interface
  never fetches anything itself**, which is why the "zero remote addresses in
  the bundle" gate still applies and still passes.

Turning the switch off means zero requests: the preview then shows the address
broken into its parts and nothing more.

The cost in dependencies: HTTPS through `native-tls`, meaning the system
certificate store. On macOS that is Security.framework, on Windows schannel;
**on Linux it needs the OpenSSL headers at build time** — the one place this
choice makes building harder on a target we do not verify anyway.

## Privacy

- Clipboard content, queries and paths never reach the logs. Logs hold operation
  identifiers, counters, times and error codes. That applies to the addresses
  fetched for link previews too.
- The interface neither opens the database nor reads arbitrary files; it talks
  to the core through a narrow set of typed commands.
- Imported HTML and code are displayed as text, never as markup.
- Export files and the `data/` directory are ignored by Git and cannot reach the
  repository or the application bundle.

## Layout

```text
apps/desktop-ui          React + TypeScript + Vite
crates/clipboard-core    entities, canonicalisation, hashing
crates/clipboard-store   SQLite, migrations, CAS, writer
crates/clipboard-search  Polish normalisation, FTS5, ranking
crates/clipboard-import  Raycast/SuperCmd parsers, import service
crates/clipboard-images  thumbnails with hard limits
src-tauri                lifecycle, IPC, permissions
tools/clipboard-import-cli  private import and verification
tools/clipboard-bench       synthetic history and scale measurements
```
