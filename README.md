# Clipboard History

Lokalny menedżer historii schowka. Rdzeń w Rust, powłoka Tauri 2, interfejs
React. Wszystko zostaje na tym urządzeniu: aplikacja nie wykonuje żadnych żądań
sieciowych związanych z historią, a czcionki i pozostałe zasoby są zapakowane
lokalnie.

## Stan

| Obszar | Stan |
|---|---|
| Model domenowy, SQLite WAL + FTS5, CAS, wyszukiwanie | gotowe |
| Importery Raycast i SuperCmd, CLI weryfikacyjne | gotowe |
| Paleta React, podglądy, akcje, kreator importu, ustawienia | gotowe |
| Skrót globalny przywołujący paletę | gotowe |
| Przechwytywanie schowka, tray, wklejanie | **niezaimplementowane** |
| Retencja, odzyskiwanie blobów, adaptery Windows i Linux | **niezaimplementowane** |

Zweryfikowana platforma: macOS (`aarch64-apple-darwin`). Windows i Linux nie
mają jeszcze adapterów systemowych.

## Wymagania

- Rust 1.96 (przypięty w `rust-toolchain.toml`)
- Node 22 i pnpm 10.33
- macOS z narzędziami wiersza poleceń Xcode (dla powłoki Tauri)

## Uruchomienie

```bash
pnpm install

# podgląd w zwykłej przeglądarce, na danych syntetycznych, bez Tauri
pnpm dev

# aplikacja natywna
pnpm tauri dev
```

`pnpm dev` używa `mockGateway` — wszystkie widoczne wpisy są wymyślone i nie
pochodzą z żadnej historii schowka.

Aby uruchomić aplikację natywną na własnej bazie deweloperskiej:

```bash
CLIPBOARD_HISTORY_DATA_DIR="data/dev" pnpm tauri dev
```

Bez tej zmiennej aplikacja używa katalogu danych systemu operacyjnego.

## Skrót globalny

`⌘⇧Space` (na innych systemach `Ctrl+Shift+Space`) przywołuje paletę i ustawia na
niej fokus; ponowne wciśnięcie ją chowa. Zamknięcie okna również tylko je chowa —
aplikacja kończy działanie wyłącznie na jawne żądanie, bo menedżer schowka, który
przestaje działać po zamknięciu okna, po cichu gubi historię.

Jeśli skrót jest już zajęty przez inną aplikację, rejestracja się nie powiedzie,
a paleta nadal działa z własnego okna. Zmiana skrótu w ustawieniach jest
zapisywana, ale zaczyna obowiązywać po ponownym uruchomieniu.

## Bramki jakości

Wszystkie muszą przechodzić przed zamknięciem zadania:

```bash
cargo fmt --all --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings

pnpm --dir apps/desktop-ui test -- --run
pnpm --dir apps/desktop-ui typecheck
pnpm --dir apps/desktop-ui build

cargo tauri build --debug --no-bundle
```

Sprawdzenie, że zbudowany pakiet nie odwołuje się do żadnego zdalnego zasobu
(dopuszczone są wyłącznie przestrzenie nazw XML, które nigdy nie są pobierane):

```bash
grep -rEoh 'https?://[^"'"'"' )]*' apps/desktop-ui/dist \
  --include='*.js' --include='*.css' --include='*.html' \
  | grep -v 'www\.w3\.org' | sort -u
```

Podpisywanie instalatorów jest poza zakresem pierwszego wydania — dlatego build
zatrzymuje się na `--no-bundle`.

## Import archiwum

Import odbywa się w całości lokalnie. Obsługiwane są eksporty Raycast i
SuperCmd, w formacie JSON (źródło prawdy) oraz CSV (format awaryjny).

```bash
cargo run -p clipboard-import-cli -- analyze --source <katalog-lub-plik>
cargo run -p clipboard-import-cli -- import  --source <katalog-lub-plik> --data-dir data/dev
cargo run -p clipboard-import-cli -- verify  --data-dir data/dev --expect-records <n>
```

`import` zapisuje wyłącznie do jawnie wskazanego katalogu danych, `verify`
otwiera bazę tylko do odczytu. Oba wypisują wyłącznie liczniki i kody stanu —
nigdy treści, ścieżek ani zapytań.

Powtórzenie tego samego importu nie tworzy duplikatów: każdy rekord źródłowy ma
stabilny odcisk, a ponowny przebieg zwraca go jako `alreadyPresent`.

### Czego import nie zrobi

- **Nie czyta bajtów spoza wybranego katalogu eksportu.** Plik eksportu jest
  niezaufanym wejściem. Wpis wskazujący plik gdzie indziej zachowuje swoją
  ścieżkę jako metadaną — interfejs pokaże lokalizację i pozwoli ją otworzyć —
  ale zawartość nie trafia do magazynu.
- **Nie zgaduje powiązań, których nie ma w danych.** Eksport SuperCmd nie
  zawiera pola wiążącego rekord obrazu z plikiem. Dopasowanie po kolejności
  plików jest zabronione, więc takie rekordy są pomijane.
- **Nie zachowuje wpisów prowadzących donikąd.** Rekord pliku lub obrazu trafia
  do historii tylko wtedy, gdy jego źródło nadal istnieje. Pozostałe są
  rozliczane jako pominięte: nic się z nimi nie stało, po prostu nie ma czego
  pokazać ani otworzyć. Suma `zaimportowane + już obecne + pominięte + błędy`
  zawsze równa się liczbie rekordów źródłowych.
- **Nie odrzuca rekordów na podstawie heurystyki sekretów.** Archiwum może
  zawierać dane wrażliwe; kreator importu ostrzega o tym przed startem.

## Prywatność

- Treść schowka, zapytania i ścieżki nigdy nie trafiają do logów. Logi zawierają
  identyfikatory operacji, liczniki, czasy i kody błędów.
- Interfejs nie otwiera bazy ani nie czyta dowolnych plików; komunikuje się z
  rdzeniem przez wąski zestaw typowanych komend.
- Zaimportowany HTML i kod są wyświetlane jako tekst, nigdy jako znaczniki.
- Pliki eksportu i katalog `data/` są ignorowane przez Git i nie mogą trafić do
  repozytorium ani do pakietu aplikacji.

## Struktura

```text
apps/desktop-ui          React + TypeScript + Vite
crates/clipboard-core    encje, kanonizacja, hashowanie
crates/clipboard-store   SQLite, migracje, CAS, writer
crates/clipboard-search  normalizacja polska, FTS5, ranking
crates/clipboard-import  parsery Raycast/SuperCmd, serwis importu
crates/clipboard-images  miniatury z twardymi limitami
src-tauri                cykl życia, IPC, uprawnienia
tools/clipboard-import-cli  prywatny import i weryfikacja
docs/superpowers         specyfikacja i plany wdrożenia
```
