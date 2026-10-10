# Trove

A local clipboard history manager. Rust core, Tauri 2 shell, React interface.
The history, its index and its blobs never leave this device, and fonts and
every other asset are bundled locally.

**Two deliberate exceptions:** the preview of a link entry fetches the page's
title and icon (turnable off in settings — see "Link previews" below), and the
optional keyvault pane reads your own secret vault when you configure it —
see "Keyvault" below. Checking for a new release is a third, and happens only
when you press **Check for updates** — see "Updates" below. Apart from those,
the application makes no network requests at all.

## Status

| Area | Status |
|---|---|
| Domain model, SQLite WAL + FTS5, CAS, search | done |
| Raycast and SuperCmd importers, verification CLI | done |
| React palette, previews, actions, import wizard, settings | done |
| Global shortcut, clipboard capture, tray, pasting | done |
| Retention and blob reclamation | done |
| Application launcher (unified search, application icons) | done |
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
TROVE_DATA_DIR="data/dev" pnpm tauri dev
```

Without that variable the application uses the operating system's data
directory.

## Global shortcut

`⌘Space` (`Ctrl+Space` elsewhere) summons the palette and focuses it; pressing
it again hides it. Closing the window also only hides it — the application
exits solely through "Quit" in the menu bar item, because a clipboard manager
that stops running when its window closes quietly loses history.

There is no Dock icon and no `⌘Tab` entry unless you ask for one. The menu bar
is where this application exists on screen: the window spends most of its life
hidden and is summoned over whatever you are working in, so by default a Dock
tile would advertise a window that is not there. **Settings → Shortcut → Show
in the Dock** turns the tile on for anyone who would rather have it — it
appears as soon as the settings are saved, clicking it summons the palette, and
the preference survives a restart. The bundle still declares `LSUIElement`, so
nothing flashes in the Dock during launch; the tile arrives a moment later,
when the setting asks for it. macOS only.

### Spotlight holds ⌘Space

macOS dispatches `⌘Space` to Spotlight before any application sees it, so the
shortcut registers cleanly and then never fires. There is no way to intercept a
chord the system has claimed — the only route is to free it, which is what the
Shortcut tab in settings offers: one button that turns Spotlight's shortcut off
and one that gives it back. Nothing is changed without being asked.

Two answers there are worth reading rather than skimming. "It takes effect
after you log out and back in" means the preference was written but the running
session did not pick it up; the shortcut works after the next login. A refusal
means nothing was written, and the same pane offers the manual route — System
Settings → Keyboard → Keyboard Shortcuts.

Some conflicts are invisible from here. Another launcher holding `⌘Space` is
not recorded in any table this application can read, so if the chord is free and
the palette still does not appear, something else is holding it.

The palette floats above other windows. It is summoned over whatever you are
working in and its whole purpose is to put something back there, so opening
behind that window would be the one place it must never open. Settings floats
too, for the same reason one step along: it is opened from the palette, and a
window that opens behind the one that opened it cannot be used. One limit worth
knowing: macOS gives a full-screen application a space of its own, and a
floating window does not follow it there.

The menu bar icon shows the history on a left click; a right click opens a menu
with pausing capture, settings and quitting. The "Pause capture" entry doubles
as the indicator: if that is what it says, the application is recording.

If the shortcut is already taken by another application, registration fails and
the settings screen says so rather than presenting a shortcut that does
nothing; the palette still opens from the menu bar. Changing the shortcut in
settings takes effect at once, and the new one is what the next launch
registers.

## Launching applications

The palette opens on its categories — Applications, Clipboard history, the
Key vault, and the Chat window — and which destination someone came for is
a fact about them, not a decision the application makes. The tiles answer
their digit (`1`–`4`), `Tab` walks the ring of lists, `⌘1`/`⌘2`/`⌘3`/`⌘4`
pick from anywhere, and clicking works too; typing from the chooser means
the history, the palette's own core. `Escape` backs out one step at a time —
first the query, then the category, back to the chooser — and hiding the
palette remains the global shortcut's job.

In the applications category the field drives the whole catalog —
alphabetically, each row with the application's own icon, its bundle
identifier when it declares one, and the folder it lives in — filtering on
the client as you type. The Key vault category lists the keys of the
paired vault by name (never their values), and `Enter` copies one through
the core; the ask crosses the network only because entering the category
asked for it. A setting — "Open the palette on its categories", on by
default — restores the single combined list where applications and history
answer one field together. `Enter` opens what is selected (an application
starts, a history entry pastes, a key lands on the clipboard) and the
palette opens on the chooser again.

The catalog is scanned lazily — never at startup — on the palette's first
opening, and every later opening re-checks the disk: the list answers
instantly from what the last scan found while a fresh scan runs in the
background beside it, and only when that scan sees an application appear or
vanish does the list swap, whole, to the new catalog. Rows never disappear
mid-refresh, and a freshly installed application shows up in the palette
that is already open. The scan covers `/Applications`,
`/System/Applications`, `~/Applications`, `/System/Library/CoreServices`
and `/Applications/Setapp` when present — two directory levels deep, so
`/Applications/Utilities` is included. Bundles that mark themselves
`LSBackgroundOnly` (in either the boolean or the string form plists ship)
are skipped everywhere: a daemon with no face at all is not an application
anyone launches by name. Menu-bar agents — bundles marked `LSUIElement` —
are listed from the folders the user owns (`/Applications`,
`~/Applications`, Setapp), where an agent is an application someone chose
to install (Raycast, Docker, a VPN living in the menu bar; Spotlight lists
these too), and skipped under `/System`, where they are the operating
system's own machinery and would flood the list with a hundred rows nobody
launches by name. Typing narrows the catalog on the
client as you type: a query matches the name's beginning, the beginning of
any of its words (split on whatever separator the bundle shipped), its
initials ("vsc" finds "Visual Studio Code"), the bundle directory's own
name when the plist display name disagrees, the characters in order
("chrm" finds "Chrome"), or the bundle identifier — best matches first,
ties alphabetical. Icons are asked for one row at a time, rendered by the
core through NSWorkspace into a small PNG, and remembered for the session;
a bundle with no icon to draw keeps the placeholder glyph.

Starting one is deliberately narrow: the path arriving from the interface must
canonicalize to an `.app` directory under a root this application scanned, and
only then does the shell spawn `/usr/bin/open -a` with that single path as an
argument vector — no shell, no new plugin, no capability grant, the same
discipline `reveal_source` already follows. A refusal answers with a stable
code (`launch_invalid`, `app_not_found`, `app_not_launchable`,
`app_outside_roots`) and never repeats the path it refused.

## Quality gates

All of these must pass before a task is closed. `scripts/ci.sh` runs them in
this order (the same gates the CI workflow ran):

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

pnpm typecheck
pnpm test
pnpm build

pnpm tauri build --debug --no-bundle
```

`git config core.hooksPath .githooks` makes every `git push` run
`scripts/ci.sh` first.

Checking that the built bundle references no remote resource at all (only XML
namespaces and the chat settings' default endpoint are allowed — the first is
never fetched, and the second is a string the core reads from the settings row
and calls only through its own client, never from the window):

```bash
grep -rEoh 'https?://[^"'"'"' )]*' apps/desktop-ui/dist \
  --include='*.js' --include='*.css' --include='*.html' \
  | grep -v 'www\.w3\.org' | grep -v 'api\.openai\.com' | sort -u
```

The gates stop at `--no-bundle`; signing, notarisation and the disk image
belong to `scripts/release.sh`, which builds and publishes a release from this
Mac — see [RELEASING.md](RELEASING.md).

## Updates

Settings → **Updates** (or **Check for updates…** in the menu bar) asks for
`latest.json` from the latest release of this repository. Nothing checks on its
own: opening the tab does not go online, the button does.

An update is installed only if `Trove.app.tar.gz` carries a minisign signature
from the key whose public half is in `src-tauri/tauri.conf.json`
(`plugins.updater.pubkey`). The archive is packed from the signed, notarised and
stapled application, so the bundle that replaces the old one passes Gatekeeper
as it is. Trove then restarts.

Releasing needs, once:

1. `pnpm tauri signer generate -w ~/.tauri/trove-updater.key` — keep the key and
   its password somewhere safe. Losing them strands every installed copy on the
   version it has.
2. The contents of `~/.tauri/trove-updater.key.pub` in `plugins.updater.pubkey`.
   The release script refuses to build while the placeholder is there.
3. The key's path in `~/.config/local-release/tauri-updater.env` and its
   password next to it — see [RELEASING.md](RELEASING.md).

Each release then publishes the disk image, `Trove.app.tar.gz`, its `.sig` and
`latest.json` on its release. 1.8.1 and earlier have no updater, and 1.9.0
looks for updates in a repository that no longer exists, so both have to be
replaced by hand from the disk image once.

## Importing an archive

Importing happens entirely locally. Raycast and SuperCmd exports are supported:
the encrypted `.rayconfig` — the file Raycast actually writes — as well as JSON
(the source of truth) and CSV (the fallback format).

```bash
cargo run -p trove-import-cli -- analyze --source <directory-or-file>
cargo run -p trove-import-cli -- import  --source <directory-or-file> --data-dir data/dev
cargo run -p trove-import-cli -- verify  --data-dir data/dev --expect-records <n>
```

### Encrypted exports

A `.rayconfig` is `IV ‖ AES-256-CBC-PKCS7(gzip(JSON))` keyed on
`SHA-256(password)`. The CLI asks for the password on the terminal without
echoing it; in a script it is supplied over `--password-stdin`:

```bash
echo "$RAYCAST_PASSWORD" | cargo run -p trove-import-cli -- import \
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
opened: the entry goes on the clipboard, the palette puts itself away, the window
you came from is brought back to the front, and Command-V is sent to it.

That last step needs the Accessibility permission (System Settings → Privacy &
Security → Accessibility). Without it the entry still reaches the clipboard and
the palette still puts itself away — what changes is that the application says
which of the three things went wrong instead of only that something did, and the
one you can fix carries a button to the setting that fixes it. It also brings
that setting up by itself, once per run: macOS shows its own permission dialog at
most once per launch and never at all once you have answered, so an application
that only asked the system to ask would, on the machine that needs this most, ask
nobody anything.

## Grouped entries

Copying the same thing again does not add a second row. The list shows one row
per distinct payload — the newest capture fronts it — with a `×N` badge for how
often it was recorded, and the preview lists when it was captured, newest
first, at most five timestamps. Older duplicate captures are pruned on ingest,
so the history really does shrink rather than merely hiding; pinned
occurrences are never pruned, and a database from before this rule is
collapsed by the same fifteen-minute maintenance pass that handles retention.

Deleting a grouped row deletes every occurrence behind it, which is what the
row promised.

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
not a curiosity. `tools/trove-bench` builds a synthetic history from a seed
— it reads no real data — and measures what a user actually feels.

```bash
cargo run --release -p trove-bench -- generate --data-dir data/bench-large --records 1000000 --seed 42
cargo run --release -p trove-bench -- measure  --data-dir data/bench-large --queries 200
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

## Chat

The chat window — the fourth tile of the palette's chooser, `⌘K` or `⌘4`
from the palette, or the Chat button in its footer — is a conversation with
one model, in a window of its own. Four providers are offered exactly:
**Z.ai** (GLM), **OpenAI**, **OpenRouter** and **Anthropic** — the first
three on the OpenAI wire, Anthropic on its own Messages wire, both handled
by the core. Each provider has its own API key field, all of them kept in
this application's own database — the same trust boundary the database
itself sits on, and the same place a keyvault token override lives; they
are not read from the keyvault, and treating a data-directory compromise
as a compromise of any key stored there is the honest rule. The model is
picked from a list: fetched live from the provider's own model endpoint,
with a standing set of suggestions when the list cannot be fetched. The
answer streams token by token, Enter sends, Shift+Enter is a newline, and
a stop button ends a long answer mid-stream.

What crosses the network is what you would expect said plainly: the
messages of the conversation, to the provider the base URL names. The key
is read from the settings row only when a request is built, and never
reaches a log line or an error message. Refusals arrive as stable codes
and are shown as fixed sentences. A conversation lives in the window's
memory: closing the window only puts it away, and quitting the application
is what ends it.

## Keyvault

The application reads a personally-run keyvault through the device's shared
agent identity at `~/.config/keyvault/agent.json` (mode 600) — one file, read by
every consumer on the machine, holding the vault address, the device-side
private JWK that opens what the vault seals, and a token per consumer:

```json
{
  "url": "https://<deployment>.convex.site",
  "privateJwk": { "kty": "RSA", "n": "...", "e": "...", "d": "...", "p": "...", "q": "..." },
  "tokens": { "mcp": "kv_...", "clipboard-history": "kv_..." }
}
```

The key is shared because a second copy is a second thing to rotate, and the
copy you forget does not announce itself — it fails as a decrypt error against
every re-sealed envelope, naming the envelope when the fault was the key. Tokens
are *not* shared for the opposite reason: one per consumer means one can be
revoked without taking the others down. A consumer the file does not name is
simply unconfigured. Set `KEYVAULT_AGENT_FILE` to put the identity elsewhere.

The settings pane offers only overrides — a vault address and a token, both
optional, for pointing one install at a different deployment. There is no field
for the private key, deliberately: it is not this application's to hold.
Unconfigured, the pane does nothing and the application stays off the network.

What the vault returns are sealed envelopes, not values: the token only
authenticates, and only this device's private key can open an answer. Copying a
secret decrypts it inside the core process and puts it straight on the
clipboard — arming the same self-write suppression the palette uses, so a
fetched key is never recorded into the history — and the interface learns only
whether it worked. Denials arrive as codes (unauthorized, out of scope, rate
limited, agent access disabled) and are shown as fixed sentences; reads are
spaced to respect the vault's per-token rate limit.

Said plainly: no key is stored by this application. The identity file is a
mode-600 file in your home directory, and if you set a token override it lands
in the settings row — a plaintext row in the local database, the same trust
boundary the database itself sits on. Treat a data-directory compromise as a
compromise of any token stored there and revoke it in the vault; the private key
is not in the database to lose. A row written by an older version, which did
keep the key there, is cleared the first time settings are saved.

## Privacy

- Clipboard content, queries and paths never reach the logs. Logs hold operation
  identifiers, counters, times and error codes. That applies to the addresses
  fetched for link previews too.
- A keyvault secret's value exists only between the decrypt and the clipboard
  write: it never enters the interface, a log line, an error message, or the
  history. The vault token and private key are never echoed anywhere either.
- The interface neither opens the database nor reads arbitrary files; it talks
  to the core through a narrow set of typed commands.
- Imported HTML and code are displayed as text, never as markup.
- Export files and the `data/` directory are ignored by Git and cannot reach the
  repository or the application bundle.

## Layout

```text
apps/desktop-ui          React + TypeScript + Vite
crates/trove-core    entities, canonicalisation, hashing
crates/trove-store   SQLite, migrations, CAS, writer
crates/trove-search  Polish normalisation, FTS5, ranking
crates/trove-import  Raycast/SuperCmd parsers, import service
crates/trove-images  thumbnails with hard limits
src-tauri                lifecycle, IPC, permissions
tools/trove-import-cli  private import and verification
tools/trove-bench       synthetic history and scale measurements
```
