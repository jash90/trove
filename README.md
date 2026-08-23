# Clipboard History

Lokalny menedżer historii schowka. Rdzeń w Rust, powłoka Tauri 2, interfejs
React. Historia, indeks i bloby nie opuszczają tego urządzenia, a czcionki i
pozostałe zasoby są zapakowane lokalnie.

**Jeden wyjątek, świadomie dodany:** podgląd wpisu z linkiem pobiera tytuł i
ikonę strony. Można go wyłączyć w ustawieniach — patrz „Podgląd stron" niżej.
Poza nim aplikacja nie wykonuje żadnych żądań sieciowych.

## Stan

| Obszar | Stan |
|---|---|
| Model domenowy, SQLite WAL + FTS5, CAS, wyszukiwanie | gotowe |
| Importery Raycast i SuperCmd, CLI weryfikacyjne | gotowe |
| Paleta React, podglądy, akcje, kreator importu, ustawienia | gotowe |
| Skrót globalny, przechwytywanie schowka, tray, wklejanie | gotowe |
| Retencja i odzyskiwanie blobów | gotowe |
| Adaptery Windows i Linux | zaimplementowane, **niezweryfikowane** |

## Macierz wsparcia

| Platforma | Stan | Co to znaczy |
|---|---|---|
| macOS | **zweryfikowane** | Uruchomione i sprawdzone na `aarch64-apple-darwin`: historia, import, przechwytywanie, skrót, tray. |
| Windows | **zaimplementowane** | Kod polityki wykluczeń i deklaracja możliwości istnieją i mają testy uruchamiane na hoście. Nie skompilowano ani nie uruchomiono na Windows. |
| Linux | **zaimplementowane** | Wykrywanie sesji X11/Wayland i deklaracja możliwości mają testy uruchamiane na hoście. Nie skompilowano ani nie uruchomiono na Linuksie. |

„Zaimplementowane" nie znaczy „działa". Na tej maszynie zainstalowany jest tylko
target `aarch64-apple-darwin`, więc nawet kompilacja krzyżowa nie została
wykonana:

```bash
rustup target add x86_64-pc-windows-msvc x86_64-unknown-linux-gnu
cargo check -p platform-windows --target x86_64-pc-windows-msvc
cargo check -p platform-linux --target x86_64-unknown-linux-gnu
```

Dopóki te polecenia nie przejdą na maszynie z odpowiednimi targetami, oba
adaptery pozostają niezweryfikowane. Wayland bez protokołu data-control nie
pozwala czytać schowka w tle w ogóle — aplikacja zgłasza wtedy jawny stan
`wayland_data_control_unavailable`, zamiast udawać historię, której nie może
zbudować.

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
aplikacja kończy działanie wyłącznie przez „Zakończ" w menu paska, bo menedżer
schowka, który przestaje działać po zamknięciu okna, po cichu gubi historię.

Ikona w pasku menu pokazuje historię lewym kliknięciem, a prawym otwiera menu z
wstrzymaniem nasłuchu, ustawieniami i wyjściem. Wpis „Wstrzymaj nasłuch" jest
zarazem wskaźnikiem: jeśli tak brzmi, aplikacja właśnie nagrywa.

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
SuperCmd: zaszyfrowany `.rayconfig` — czyli plik, który Raycast faktycznie
zapisuje — oraz JSON (źródło prawdy) i CSV (format awaryjny).

```bash
cargo run -p clipboard-import-cli -- analyze --source <katalog-lub-plik>
cargo run -p clipboard-import-cli -- import  --source <katalog-lub-plik> --data-dir data/dev
cargo run -p clipboard-import-cli -- verify  --data-dir data/dev --expect-records <n>
```

### Eksport zaszyfrowany

`.rayconfig` to `IV ‖ AES-256-CBC-PKCS7(gzip(JSON))` z kluczem `SHA-256(hasło)`.
CLI pyta o hasło na terminalu, nie wyświetlając go; w skrypcie podaje się je
przez `--password-stdin`:

```bash
echo "$RAYCAST_PASSWORD" | cargo run -p clipboard-import-cli -- import \
  --source "Raycast 2026-08-22 14.39.05.rayconfig" --data-dir data/dev --password-stdin
```

**Nie ma flagi przyjmującej hasło jako argument** — argumenty procesu widzi każdy
przez `ps` i zapisuje je historia powłoki. Hasło nie jest nigdzie zapisywane ani
logowane, a odszyfrowana treść nigdy nie trafia na dysk: jest strumieniowana
prosto do parsera. Potrzebne jest dokładnie raz, przy analizie, bo dalej
pracujemy już na sparsowanych rekordach.

Gdy katalog zawiera i zwykły manifest, i `.rayconfig`, wygrywa zwykły — żeby nie
pytać o hasło, kiedy nie jest potrzebne.

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
- **Nie potrafi orzec, że hasło było poprawne.** AES-CBC nie ma znacznika
  uwierzytelniającego, więc importer odrzuca tylko to, co jawnie błędne:
  dopełnienie PKCS7 i sygnaturę gzip. Obie kontrole razem stanowią całe
  wykrywanie złego hasła.

## Przechwytywanie i wklejanie

Aplikacja nagrywa to, co kopiujesz, dopóki nasłuch nie zostanie wstrzymany.
Odrzuca — zanim cokolwiek przeczyta — wpisy oznaczone przez aplikację źródłową
jako `ConcealedType` lub `TransientType`; tak oznaczają swoje wpisy menedżery
haseł. Odrzuca też wszystko z aplikacji na liście wykluczeń.

`Enter` i `⌘⇧V` proszą o wklejenie do okna, w którym byłeś przed otwarciem
palety. Wymaga to uprawnienia Accessibility (Ustawienia systemowe → Prywatność i
ochrona → Dostępność). Bez niego wpis i tak trafia do schowka, a aplikacja mówi,
dlaczego nie wkleiła, zamiast milczeć.

## Retencja i miejsce na dysku

Historia jest domyślnie nieograniczona. Włączenie retencji w ustawieniach trwale
usuwa wpisy starsze niż podana liczba dni — z wyjątkiem przypiętych, bo
przypięcie to jawne „zachowaj to". Sprzątanie działa w małych partiach co
kwadrans, żeby nigdy nie blokować nagrywania.

Bloby, do których nic już nie odsyła, są zwalniane w tym samym przebiegu.
Skanowanie tylko obserwuje; o tym, czy blob naprawdę jest nieużywany, decyduje
writer tuż przed usunięciem, bo między skanem a usunięciem import mógł zacząć go
używać.

## Wydajność przy dużej historii

Historia jest nieograniczona, więc „czy to jeszcze działa przy milionie wpisów"
jest bramką, nie ciekawostką. `tools/clipboard-bench` buduje syntetyczną historię
z ziarna — nie czyta żadnych prawdziwych danych — i mierzy to, co użytkownik
odczuwa.

```bash
cargo run --release -p clipboard-bench -- generate --data-dir data/bench-large --records 1000000 --seed 42
cargo run --release -p clipboard-bench -- measure  --data-dir data/bench-large --queries 200
```

Pomiar z 23 sierpnia 2026, macOS na `aarch64-apple-darwin`, milion rekordów,
baza 772 MB + 307 MB blobów:

| Pomiar | Wynik | Budżet |
|---|---|---|
| Wyszukiwanie selektywne, p95 | 3,2 ms | 50 ms |
| Wyszukiwanie selektywne z otwartym czytnikiem, p95 | 0,9 ms | — |
| Pierwsza strona listy (od skrótu do wyników) | 2,8 ms | 100 ms |
| Przewijanie: pierwsza strona → 400. strona | 2,6 ms → 3,4 ms | bez wzrostu |
| Jeden ograniczony przebieg odzyskiwania blobów | 100–125 ms | ograniczony |
| RSS aplikacji z bazą miliona rekordów | 131 MB (build debug) | 150 MB |
| Zapis podczas generowania | ok. 499 rekordów/s | — |

Przewijanie nie zwalnia z głębokością, bo paginacja idzie po kluczu
`(captured_at_ms, event_id)`, a nie po rosnącym `OFFSET`.

**Znany limit.** Zapytanie o słowo, które zawiera duża część historii, kosztuje
setki milisekund — 811 ms p95 dla terminu pasującego do 600 tys. rekordów.
Trafności nie da się ustalić bez policzenia punktacji dla każdego dopasowania.
W syntetycznym zbiorze każdy rekord powstaje z dwunastowyrazowego słownika, więc
nawet najrzadsze słowo trafia w 8% bazy; prawdziwa historia ma długi ogon słów i
tego przypadku praktycznie nie produkuje. Budżet 50 ms jest dotrzymany dla
zapytań selektywnych i **nie** jest dotrzymany dla terminów masowych.

## Podgląd stron

Wpis z linkiem pokazuje domenę, ścieżkę oraz — gdy pobieranie jest włączone —
tytuł i ikonę strony. **To jedyne miejsce, w którym aplikacja łączy się z
siecią.**

Co to kosztuje, powiedziane wprost: przy włączonym pobieraniu otwarcie palety
odpytuje strony widoczne na liście, więc każda z tych domen dowiaduje się, że
w tej chwili zaglądasz do swojego schowka — razem z Twoim adresem IP. Wynik jest
zapamiętywany na stałe, także nieudany, więc ta sama strona jest pytana raz.

Granice, które obowiązują zawsze:

- tylko `http` i `https`; żaden inny schemat nie jest pobierany,
- **tylko porty 80 i 443** — inaczej wpis w schowku byłby sposobem na pukanie do
  każdej usługi, jaką ta maszyna widzi,
- **nigdy** adresy lokalne i prywatne (`localhost`, `0.0.0.0/8`, `127.0.0.0/8`,
  `10/8`, `172.16/12`, `192.168/16`, `169.254/16`, `.local`, ULA IPv6, a także
  adresy IPv4 przemycone w IPv6 przez 6to4, Teredo i NAT64) — sprawdzane zarówno
  w nazwie, jak i w adresie zwróconym przez DNS, **po każdym przekierowaniu**,
  żeby podgląd nie stał się skanerem Twojej sieci,
- sprawdzony adres jest **przypinany do połączenia**, więc nazwa nie może
  odpowiedzieć czym innym pomiędzy sprawdzeniem a połączeniem,
- twarde limity czasu, rozmiaru odpowiedzi i liczby przekierowań; bez ciasteczek
  i bez wykonywania JavaScriptu,
- czytany jest wyłącznie `<title>` i odnośnik do ikony; nic więcej nie jest
  parsowane ani przechowywane,
- ikona trafia do okna jako bajty z lokalnego magazynu — **interfejs nigdy nie
  pobiera niczego sam**, dlatego bramka „zero zdalnych adresów w pakiecie" nadal
  obowiązuje i nadal przechodzi.

Wyłączenie przełącznika oznacza zero żądań: podgląd pokazuje wtedy sam rozbiór
adresu.

Koszt w zależnościach: HTTPS przez `native-tls`, czyli magazyn certyfikatów
systemu. Na macOS to Security.framework, na Windows schannel; **na Linuksie
wymaga nagłówków OpenSSL przy budowaniu** — to jedyne miejsce, w którym ta
zmiana utrudnia budowę na targecie, którego i tak nie weryfikujemy.

## Prywatność

- Treść schowka, zapytania i ścieżki nigdy nie trafiają do logów. Logi zawierają
  identyfikatory operacji, liczniki, czasy i kody błędów. Dotyczy to również
  adresów pobieranych na potrzeby podglądu stron.
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
tools/clipboard-bench       syntetyczna historia i pomiary skali
docs/superpowers         specyfikacja i plany wdrożenia
```
