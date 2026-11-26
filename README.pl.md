# Trove

[English](README.md) | **Polski**

Lokalny menedżer historii schowka. Rdzeń w Rust, powłoka Tauri 2, interfejs
w React. Historia, jej indeks i bloby nigdy nie opuszczają tego urządzenia,
a czcionki i wszystkie pozostałe zasoby są dołączone lokalnie.

**Dwa świadome wyjątki:** podgląd wpisu z linkiem pobiera tytuł i ikonę strony
(można to wyłączyć w ustawieniach — zob. „Podgląd linków” niżej), a opcjonalny
panel keyvault czyta Twój własny sejf sekretów, jeśli go skonfigurujesz — zob.
„Keyvault” niżej. Trzecim jest sprawdzanie nowej wersji, które odbywa się
wyłącznie po naciśnięciu **Sprawdź aktualizacje** — zob. „Aktualizacje” niżej.
Poza tym aplikacja nie wykonuje żadnych zapytań sieciowych.

> Wersja angielska ([README.md](README.md)) jest wersją główną; w razie
> rozbieżności rozstrzyga ona.

## Stan

| Obszar | Stan |
|---|---|
| Model domeny, SQLite WAL + FTS5, CAS, wyszukiwanie | gotowe |
| Importery Raycast i SuperCmd, CLI weryfikujące | gotowe |
| Paleta w React, podglądy, akcje, kreator importu, ustawienia | gotowe |
| Globalny skrót, przechwytywanie schowka, ikona w pasku menu, wklejanie | gotowe |
| Retencja i odzyskiwanie blobów | gotowe |
| Uruchamianie aplikacji (wspólne wyszukiwanie, ikony aplikacji) | gotowe |
| Adaptery Windows i Linux | zaimplementowane, **niezweryfikowane** |

## Obsługiwane platformy

| Platforma | Stan | Co to znaczy |
|---|---|---|
| macOS | **zweryfikowana** | Uruchomiona i sprawdzona na `aarch64-apple-darwin`: historia, import, przechwytywanie, skrót, pasek menu. |
| Windows | **zaimplementowana** | Kod polityki wykluczeń i deklaracja możliwości istnieją i mają testy uruchamiane na hoście. Nigdy nie skompilowana ani nie uruchomiona na Windows. |
| Linux | **zaimplementowana** | Wykrywanie sesji X11/Wayland i deklaracja możliwości mają testy uruchamiane na hoście. Nigdy nie skompilowana ani nie uruchomiona na Linuksie. |

„Zaimplementowana” nie znaczy „działa”. Na tej maszynie zainstalowany jest tylko
target `aarch64-apple-darwin`, więc nie wykonano nawet kompilacji skrośnej:

```bash
rustup target add x86_64-pc-windows-msvc x86_64-unknown-linux-gnu
cargo check -p platform-windows --target x86_64-pc-windows-msvc
cargo check -p platform-linux --target x86_64-unknown-linux-gnu
```

Dopóki te polecenia nie przejdą na maszynie z właściwymi targetami, oba adaptery
pozostają niezweryfikowane. Wayland bez protokołu data-control w ogóle nie
pozwala czytać schowka w tle — aplikacja zgłasza wtedy jawny stan
`wayland_data_control_unavailable`, zamiast udawać historię, której nie jest
w stanie zbudować.

## Wymagania

- Rust 1.96 (przypięty w `rust-toolchain.toml`)
- Node 22 i pnpm 10.33
- macOS z narzędziami wiersza poleceń Xcode (dla powłoki Tauri)

## Uruchamianie

```bash
pnpm install

# podgląd w zwykłej przeglądarce, na danych syntetycznych, bez Tauri
pnpm dev

# natywna aplikacja
pnpm tauri dev
```

`pnpm dev` korzysta z `mockGateway` — każdy widoczny wpis jest zmyślony i nie
pochodzi z żadnej historii schowka.

Aby uruchomić natywną aplikację na własnej bazie deweloperskiej:

```bash
TROVE_DATA_DIR="data/dev" pnpm tauri dev
```

Bez tej zmiennej aplikacja używa katalogu danych systemu operacyjnego.

## Globalny skrót

`⌘Spacja` (`Ctrl+Spacja` na innych systemach) przywołuje paletę i nadaje jej
fokus; ponowne naciśnięcie ją chowa. Zamknięcie okna również tylko je chowa —
aplikację kończy wyłącznie „Zakończ” w ikonie paska menu, bo menedżer schowka,
który przestaje działać po zamknięciu okna, po cichu gubi historię.

Nie ma ikony w Docku ani pozycji w `⌘Tab`, chyba że o nią poprosisz. Pasek menu
to miejsce, w którym ta aplikacja istnieje na ekranie: okno przez większość
czasu jest ukryte i przywoływane nad tym, w czym akurat pracujesz, więc
domyślny kafelek w Docku reklamowałby okno, którego nie ma. **Ustawienia →
Skrót → Pokaż w Docku** włącza kafelek dla tych, którzy wolą go mieć — pojawia
się, gdy tylko ustawienia zostaną zapisane, kliknięcie go przywołuje paletę,
a preferencja przetrwa restart. Pakiet nadal deklaruje `LSUIElement`, więc
w trakcie uruchamiania nic nie mignie w Docku; kafelek pojawia się chwilę
później, gdy prosi o niego ustawienie. Tylko macOS.

### Spotlight zajmuje ⌘Spację

macOS przekazuje `⌘Spację` do Spotlight, zanim zobaczy ją jakakolwiek
aplikacja, więc skrót rejestruje się poprawnie, a potem nigdy nie działa. Nie da
się przechwycić kombinacji zajętej przez system — jedyna droga to ją zwolnić
i to właśnie oferuje karta Skrót w ustawieniach: jeden przycisk wyłącza skrót
Spotlight, drugi go przywraca. Nic nie zmienia się bez pytania.

Dwie odpowiedzi warto przeczytać, a nie przelecieć wzrokiem. „Zadziała po
wylogowaniu i ponownym zalogowaniu” oznacza, że preferencja została zapisana,
ale bieżąca sesja jej nie wczytała; skrót zadziała po następnym logowaniu.
Odmowa oznacza, że nic nie zostało zapisane, a ten sam panel podaje drogę
ręczną — Ustawienia systemowe → Klawiatura → Skróty klawiszowe.

Niektórych konfliktów stąd nie widać. Inny launcher trzymający `⌘Spację` nie
jest zapisany w żadnej tabeli, którą ta aplikacja może odczytać, więc jeśli
kombinacja jest wolna, a paleta i tak się nie pojawia, trzyma ją coś innego.

Paleta unosi się nad innymi oknami. Jest przywoływana nad tym, w czym pracujesz,
a cały jej sens to coś tam z powrotem wstawić, więc otwarcie się za tym oknem
byłoby jedynym miejscem, w którym nie może się otworzyć. Ustawienia też się
unoszą, z tego samego powodu o krok dalej: otwiera się je z palety, a okno
otwierające się za oknem, które je otworzyło, jest bezużyteczne. Jedno
ograniczenie warto znać: macOS daje aplikacji pełnoekranowej osobną przestrzeń,
a pływające okno za nią nie podąża.

Ikona w pasku menu pokazuje historię po kliknięciu lewym przyciskiem; prawy
otwiera menu z wstrzymaniem przechwytywania, ustawieniami i zakończeniem.
Pozycja „Wstrzymaj przechwytywanie” jest zarazem wskaźnikiem: jeśli tak brzmi,
aplikacja nagrywa.

Jeśli skrót jest już zajęty przez inną aplikację, rejestracja się nie udaje,
a ekran ustawień mówi o tym wprost, zamiast pokazywać skrót, który nic nie robi;
paleta nadal otwiera się z paska menu. Zmiana skrótu w ustawieniach działa od
razu, a nowy skrót jest tym, który zarejestruje następne uruchomienie.

## Uruchamianie aplikacji

Paleta otwiera się na swoich kategoriach — Aplikacje, Historia schowka, Sejf
kluczy i okno Czatu — a to, po co ktoś przyszedł, jest faktem o nim, a nie
decyzją aplikacji. Kafelki odpowiadają na swoją cyfrę (`1`–`4`), `Tab` obchodzi
pierścień list, `⌘1`/`⌘2`/`⌘3`/`⌘4` wybierają z dowolnego miejsca, działa też
kliknięcie; pisanie na ekranie wyboru oznacza historię, czyli rdzeń palety.
`Escape` cofa się krok po kroku — najpierw zapytanie, potem kategoria, z
powrotem do wyboru — a chowanie palety pozostaje zadaniem globalnego skrótu.

W kategorii aplikacji pole steruje całym katalogiem — alfabetycznie, każdy
wiersz z ikoną aplikacji, jej identyfikatorem pakietu, jeśli go deklaruje,
i folderem, w którym leży — filtrowanym po stronie klienta w trakcie pisania.
Kategoria Sejf kluczy wymienia klucze sparowanego sejfu po nazwie (nigdy po
wartości), a `Enter` kopiuje klucz przez rdzeń; zapytanie idzie przez sieć
tylko dlatego, że wejście do kategorii o nie poprosiło. Ustawienie „Otwieraj
paletę na kategoriach”, domyślnie włączone, przywraca jedną wspólną listę,
w której aplikacje i historia odpowiadają na jedno pole. `Enter` otwiera to, co
zaznaczone (aplikacja startuje, wpis historii się wkleja, klucz trafia do
schowka), a paleta znów otwiera się na ekranie wyboru.

Katalog skanowany jest leniwie — nigdy przy starcie — przy pierwszym otwarciu
palety, a każde kolejne otwarcie ponownie sprawdza dysk: lista odpowiada od
razu tym, co znalazł ostatni skan, a obok w tle biegnie świeży skan i dopiero
gdy zobaczy, że jakaś aplikacja się pojawiła lub zniknęła, lista podmienia się
w całości na nowy katalog. Wiersze nigdy nie znikają w trakcie odświeżania,
a świeżo zainstalowana aplikacja pojawia się w palecie, która jest już otwarta.
Skan obejmuje `/Applications`, `/System/Applications`, `~/Applications`,
`/System/Library/CoreServices` oraz `/Applications/Setapp`, jeśli istnieje —
dwa poziomy katalogów w głąb, więc `/Applications/Utilities` też się liczy.
Pakiety oznaczające się jako `LSBackgroundOnly` (w formie logicznej lub
tekstowej, obie występują w plistach) są pomijane wszędzie: demon bez żadnego
interfejsu nie jest aplikacją, którą ktoś uruchamia po nazwie. Agenci paska
menu — pakiety oznaczone `LSUIElement` — są wymieniani z folderów należących do
użytkownika (`/Applications`, `~/Applications`, Setapp), gdzie agent jest
aplikacją, którą ktoś świadomie zainstalował (Raycast, Docker, VPN w pasku
menu; Spotlight też je pokazuje), i pomijani pod `/System`, gdzie są
mechanizmami samego systemu i zalałyby listę setką wierszy, których nikt nie
uruchamia po nazwie. Pisanie zawęża katalog po stronie klienta: zapytanie
pasuje do początku nazwy, początku dowolnego jej słowa (podzielonego na
dowolnym separatorze, jaki dał pakiet), jej inicjałów („vsc” znajduje „Visual
Studio Code”), nazwy katalogu pakietu, gdy nazwa wyświetlana z plisty się
różni, kolejnych znaków („chrm” znajduje „Chrome”) albo identyfikatora pakietu
— najlepsze dopasowania najpierw, remisy alfabetycznie. O ikony prosi się po
jednym wierszu naraz, rdzeń renderuje je przez NSWorkspace do małego PNG
i zapamiętuje na czas sesji; pakiet bez ikony do narysowania zachowuje glif
zastępczy.

Uruchamianie jest celowo wąskie: ścieżka przychodząca z interfejsu musi po
kanonikalizacji wskazywać katalog `.app` pod korzeniem, który ta aplikacja
przeskanowała, i dopiero wtedy powłoka uruchamia `/usr/bin/open -a` z tą jedną
ścieżką jako wektorem argumentów — bez powłoki systemowej, bez nowej wtyczki,
bez nadawania uprawnień, w tej samej dyscyplinie, której już trzyma się
`reveal_source`. Odmowa odpowiada stabilnym kodem (`launch_invalid`,
`app_not_found`, `app_not_launchable`, `app_outside_roots`) i nigdy nie powtarza
ścieżki, którą odrzuciła.

## Bramki jakości

Wszystkie muszą przejść przed zamknięciem zadania. `scripts/ci.sh` uruchamia je
w tej kolejności (to te same bramki, które uruchamiał workflow CI):

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

pnpm typecheck
pnpm test
pnpm build

pnpm tauri build --debug --no-bundle
```

`git config core.hooksPath .githooks` sprawia, że każdy `git push` najpierw
uruchamia `scripts/ci.sh`.

Sprawdzenie, że zbudowany pakiet nie odwołuje się do żadnego zdalnego zasobu
(dozwolone są tylko przestrzenie nazw XML i domyślny endpoint ustawień czatu —
pierwszych nikt nigdy nie pobiera, a drugi to napis, który rdzeń czyta z wiersza
ustawień i wywołuje wyłącznie przez własnego klienta, nigdy z okna):

```bash
grep -rEoh 'https?://[^"'"'"' )]*' apps/desktop-ui/dist \
  --include='*.js' --include='*.css' --include='*.html' \
  | grep -v 'www\.w3\.org' | grep -v 'api\.openai\.com' | sort -u
```

Bramki kończą się na `--no-bundle`; podpisywanie, notaryzacja i obraz dysku
należą do `scripts/release.sh`, który buduje i publikuje wydanie z tego Maca —
zob. [RELEASING.md](RELEASING.md) (po angielsku).

## Aktualizacje

Ustawienia → **Aktualizacje** (albo **Sprawdź aktualizacje…** w pasku menu)
pobiera `latest.json` z najnowszego wydania tego repozytorium. Nic nie sprawdza
samo z siebie: otwarcie karty nie łączy się z siecią, robi to przycisk.

Aktualizacja instaluje się tylko wtedy, gdy `Trove.app.tar.gz` ma podpis
minisign kluczem, którego publiczna połowa jest w `src-tauri/tauri.conf.json`
(`plugins.updater.pubkey`). Archiwum pakowane jest z podpisanej,
notaryzowanej i ostemplowanej (stapled) aplikacji, więc pakiet zastępujący
stary przechodzi Gatekeepera w takiej postaci, w jakiej jest. Następnie Trove
się restartuje.

Wydawanie wymaga jednorazowo:

1. `pnpm tauri signer generate -w ~/.tauri/trove-updater.key` — przechowuj klucz
   i jego hasło w bezpiecznym miejscu. Ich utrata zostawia każdą zainstalowaną
   kopię na wersji, którą ma.
2. Zawartości `~/.tauri/trove-updater.key.pub` w `plugins.updater.pubkey`.
   Skrypt wydania odmawia budowania, dopóki jest tam placeholder.
3. Ścieżki klucza w `~/.config/local-release/tauri-updater.env` i jego hasła
   obok niego — zob. [RELEASING.md](RELEASING.md).

Każde wydanie publikuje następnie obraz dysku, `Trove.app.tar.gz`, jego `.sig`
i `latest.json`. Wersje 1.8.1 i starsze nie mają aktualizatora, a 1.9.0 szuka
aktualizacji w repozytorium, które już nie istnieje, więc obie trzeba raz
zastąpić ręcznie z obrazu dysku.

## Import archiwum

Import odbywa się w całości lokalnie. Obsługiwane są eksporty Raycast
i SuperCmd: zaszyfrowany `.rayconfig` — plik, który Raycast faktycznie zapisuje
— a także JSON (źródło prawdy) i CSV (format zapasowy).

```bash
cargo run -p trove-import-cli -- analyze --source <katalog-lub-plik>
cargo run -p trove-import-cli -- import  --source <katalog-lub-plik> --data-dir data/dev
cargo run -p trove-import-cli -- verify  --data-dir data/dev --expect-records <n>
```

### Zaszyfrowane eksporty

`.rayconfig` to `IV ‖ AES-256-CBC-PKCS7(gzip(JSON))` z kluczem
`SHA-256(hasło)`. CLI pyta o hasło w terminalu bez jego wyświetlania; w skrypcie
podaje się je przez `--password-stdin`:

```bash
echo "$RAYCAST_PASSWORD" | cargo run -p trove-import-cli -- import \
  --source "Raycast 2026-08-22 14.39.05.rayconfig" --data-dir data/dev --password-stdin
```

**Nie ma flagi przyjmującej hasło jako argument** — argumenty procesu widzi każdy
przez `ps`, a historia powłoki je zapisuje. Hasło nigdy nie jest zapisywane ani
logowane, a odszyfrowana treść nigdy nie trafia na dysk: jest strumieniowana
prosto do parsera. Potrzebne jest dokładnie raz, podczas analizy, bo wszystko
potem działa na sparsowanych rekordach.

Gdy katalog zawiera zarówno jawny manifest, jak i `.rayconfig`, wygrywa jawny —
żeby nie pytać o hasło, gdy nie jest potrzebne.

`import` zapisuje wyłącznie do katalogu danych, który został mu jawnie podany,
a `verify` otwiera bazę tylko do odczytu. Oba wypisują wyłącznie liczniki i kody
stanu — nigdy treści, ścieżek ani zapytań.

Powtórzenie tego samego importu nie tworzy duplikatów: każdy rekord źródłowy ma
stabilny odcisk, a drugie uruchomienie raportuje go jako `alreadyPresent`.

### Czego import nie zrobi

- **Nie czyta bajtów spoza wybranego katalogu eksportu.** Plik eksportu to
  niezaufane wejście. Wpis wskazujący plik gdzie indziej zachowuje ścieżkę jako
  metadane — interfejs pokaże lokalizację i pozwoli ją otworzyć — ale treść nie
  trafia do magazynu.
- **Nie zgaduje powiązań, których dane nie zawierają.** Eksport SuperCmd nie ma
  pola łączącego rekord obrazu z plikiem. Dopasowywanie po kolejności plików jest
  zabronione, więc takie rekordy są pomijane.
- **Nie zachowuje wpisów prowadzących donikąd.** Rekord pliku lub obrazu trafia
  do historii tylko wtedy, gdy jego źródło nadal istnieje. Reszta jest
  rozliczana jako pominięta: nic im się nie stało, po prostu nie ma czego pokazać
  ani otworzyć. `zaimportowane + już obecne + pominięte + nieudane` zawsze równa
  się liczbie rekordów źródłowych.
- **Nie odrzuca rekordów na podstawie heurystyki wykrywania sekretów.** Archiwum
  może zawierać dane wrażliwe; kreator importu ostrzega o tym przed startem.
- **Nie potrafi powiedzieć, że hasło było poprawne.** AES-CBC nie ma znacznika
  uwierzytelniającego, więc importer odrzuca tylko to, co jest ewidentnie złe:
  dopełnienie PKCS7 i sygnaturę gzip. Te dwa sprawdzenia razem to całe jego
  wykrywanie błędnego hasła.

## Przechwytywanie i wklejanie

Aplikacja zapisuje to, co kopiujesz, dopóki przechwytywanie nie jest wstrzymane.
Odrzuca — zanim cokolwiek przeczyta — wpisy, które aplikacja źródłowa oznaczyła
jako `ConcealedType` lub `TransientType`; tak swoje wpisy oznaczają menedżery
haseł. Odrzuca też wszystko z aplikacji znajdujących się na liście wykluczeń.

`Enter` i `⌘⇧V` proszą o wklejenie do okna, w którym byłeś przed otwarciem
palety: wpis trafia do schowka, paleta się chowa, okno, z którego przyszedłeś,
wraca na wierzch i dostaje Command-V.

Ten ostatni krok wymaga uprawnienia Dostępność (Ustawienia systemowe →
Prywatność i ochrona → Dostępność). Bez niego wpis i tak trafia do schowka,
a paleta i tak się chowa — zmienia się to, że aplikacja mówi, która z trzech
rzeczy się nie udała, zamiast tylko tego, że coś się nie udało, a ta, którą
możesz naprawić, ma przycisk prowadzący do ustawienia, które ją naprawia.
Aplikacja sama też wywołuje to ustawienie, raz na uruchomienie: macOS pokazuje
własne okno uprawnień najwyżej raz na uruchomienie i wcale, jeśli już
odpowiedziałeś, więc aplikacja, która tylko prosi system o zapytanie, na
maszynie, która tego najbardziej potrzebuje, nikogo o nic by nie zapytała.

## Wpisy grupowane

Ponowne skopiowanie tego samego nie dodaje drugiego wiersza. Lista pokazuje
jeden wiersz na każdą odrębną treść — na przodzie najnowsze przechwycenie —
z plakietką `×N` mówiącą, ile razy ją zapisano, a podgląd wymienia, kiedy ją
przechwycono, od najnowszych, najwyżej pięć znaczników czasu. Starsze
zduplikowane przechwycenia są przycinane przy zapisie, więc historia naprawdę
się kurczy, a nie tylko chowa; przypięte wystąpienia nigdy nie są przycinane,
a baza sprzed tej reguły jest zwijana przez ten sam piętnastominutowy przebieg
konserwacji, który obsługuje retencję.

Usunięcie zgrupowanego wiersza usuwa każde stojące za nim wystąpienie — to
właśnie obiecywał wiersz.

## Retencja i miejsce na dysku

Historia domyślnie jest nieograniczona. Włączenie retencji w ustawieniach trwale
usuwa wpisy starsze niż podana liczba dni — z wyjątkiem przypiętych, bo
przypięcie to jawne „zachowaj to”. Czyszczenie biegnie małymi porcjami co
kwadrans, więc nigdy nie blokuje zapisywania.

W tym samym przebiegu zwalniane są bloby, do których nic już się nie odwołuje.
Skan tylko obserwuje; o tym, czy blob naprawdę jest nieużywany, decyduje
zapisujący tuż przed usunięciem, bo między skanem a usunięciem mógł zacząć go
używać import.

## Wydajność przy dużej historii

Historia jest nieograniczona, więc „czy to nadal działa przy milionie wpisów”
to bramka, a nie ciekawostka. `tools/trove-bench` buduje syntetyczną historię
z ziarna — nie czyta żadnych prawdziwych danych — i mierzy to, co użytkownik
faktycznie odczuwa.

```bash
cargo run --release -p trove-bench -- generate --data-dir data/bench-large --records 1000000 --seed 42
cargo run --release -p trove-bench -- measure  --data-dir data/bench-large --queries 200
```

Zmierzone 23 sierpnia 2026, macOS na `aarch64-apple-darwin`, milion rekordów,
baza 772 MB plus 307 MB blobów:

| Pomiar | Wynik | Budżet |
|---|---|---|
| Selektywne wyszukiwanie, p95 | 3,2 ms | 50 ms |
| Selektywne wyszukiwanie przy otwartym czytniku, p95 | 0,9 ms | — |
| Pierwsza strona listy (od skrótu do wyników) | 2,8 ms | 100 ms |
| Przewijanie: pierwsza strona → 400. strona | 2,6 ms → 3,4 ms | bez wzrostu |
| Jeden ograniczony przebieg odzyskiwania blobów | 100–125 ms | ograniczony |
| RSS aplikacji z bazą miliona rekordów | 131 MB (build debug) | 150 MB |
| Przepustowość zapisu podczas generowania | ok. 499 rekordów/s | — |

Przewijanie nie zwalnia wraz z głębokością, bo stronicowanie idzie po kluczu
`(captured_at_ms, event_id)`, a nie po rosnącym `OFFSET`.

**Znane ograniczenie.** Zapytanie o słowo zawarte w dużej części historii
kosztuje setki milisekund — 811 ms p95 dla terminu pasującego do 600 tysięcy
rekordów. Trafności nie da się ustalić bez ocenienia każdego dopasowania.
W zbiorze syntetycznym każdy rekord zbudowany jest ze słownika dwunastu słów,
więc nawet najrzadsze słowo trafia w 8% bazy; prawdziwa historia ma długi ogon
słów i ten przypadek zdarza się w niej rzadko. Budżet 50 ms jest dotrzymany dla
zapytań selektywnych i **nie** jest dotrzymany dla terminów masowych.

## Podgląd linków

Wpis z linkiem pokazuje domenę, ścieżkę i — gdy pobieranie jest włączone —
tytuł i ikonę strony. **To jedyne miejsce, w którym aplikacja rozmawia
z siecią.**

Ile to kosztuje, mówiąc wprost: przy włączonym pobieraniu otwarcie palety
odpytuje strony widoczne na liście, więc każda z tych domen dowiaduje się, że
właśnie przeglądasz schowek — razem z Twoim adresem IP. Wynik jest zapamiętywany
na stałe, także porażki, więc o tę samą stronę pyta się raz.

Granice, które obowiązują zawsze:

- tylko `http` i `https`; żaden inny schemat nie jest pobierany,
- **tylko porty 80 i 443** — inaczej wpis w schowku byłby sposobem na pukanie do
  każdej usługi, którą widzi ta maszyna,
- **nigdy** adresy lokalne i prywatne (`localhost`, `0.0.0.0/8`, `127.0.0.0/8`,
  `10/8`, `172.16/12`, `192.168/16`, `169.254/16`, `.local`, IPv6 ULA oraz
  adresy IPv4 przemycane w IPv6 przez 6to4, Teredo i NAT64) — sprawdzane
  zarówno w nazwie, jak i w adresie zwróconym przez DNS, **po każdym
  przekierowaniu**, żeby podgląd nie stał się skanerem Twojej sieci,
- sprawdzony adres jest **przypięty do połączenia**, więc nazwa nie może
  odpowiedzieć czymś innym między sprawdzeniem a połączeniem,
- twarde limity czasu, rozmiaru odpowiedzi i liczby przekierowań; bez ciasteczek
  i bez wykonywania JavaScriptu,
- czytane są tylko `<title>`, odnośnik do ikony i `og:image` (obraz, który
  strona sama wskazuje); nic innego nie jest parsowane ani zapisywane,
- link prowadzący wprost do obrazu jest rozpoznawany po typie odpowiedzi i sam
  staje się miniaturą,
- obraz strony jest zmniejszany przed zapisaniem, żeby historia linków nie
  zamieniła się w archiwum zdjęć,
- czytana jest tylko sekcja `<head>` — do `</head>`, ani bajtu dalej,
- ikona dociera do okna jako bajty z lokalnego magazynu — **interfejs nigdy nic
  sam nie pobiera**, dlatego bramka „zero zdalnych adresów w pakiecie” nadal
  obowiązuje i nadal przechodzi.

Wyłączenie przełącznika oznacza zero zapytań: podgląd pokazuje wtedy adres
rozłożony na części i nic więcej.

Koszt w zależnościach: HTTPS przez `native-tls`, czyli systemowy magazyn
certyfikatów. Na macOS to Security.framework, na Windows schannel; **na Linuksie
wymaga nagłówków OpenSSL w czasie budowania** — jedyne miejsce, w którym ten
wybór utrudnia budowanie, i to na targecie, którego i tak nie weryfikujemy.

## Czat

Okno czatu — czwarty kafelek ekranu wyboru palety, `⌘K` lub `⌘4` z palety albo
przycisk Czat w jej stopce — to rozmowa z jednym modelem, w osobnym oknie.
Dostępni są dokładnie czterej dostawcy: **Z.ai** (GLM), **OpenAI**,
**OpenRouter** i **Anthropic** — trzej pierwsi przez protokół OpenAI, Anthropic
przez własny protokół Messages; oba obsługuje rdzeń. Każdy dostawca ma własne
pole klucza API, wszystkie trzymane we własnej bazie tej aplikacji — na tej
samej granicy zaufania, na której stoi sama baza, i w tym samym miejscu, w
którym żyje nadpisany token keyvault; nie są czytane z keyvault, a uczciwa
zasada brzmi: przejęcie katalogu danych to przejęcie każdego zapisanego tam
klucza. Model wybiera się z listy: pobieranej na żywo z endpointu modeli
dostawcy, a gdy listy nie da się pobrać — ze stałego zestawu podpowiedzi.
Odpowiedź spływa token po tokenie, Enter wysyła, Shift+Enter to nowa linia,
a przycisk stop kończy długą odpowiedź w trakcie strumienia.

Przez sieć idzie to, czego można się spodziewać: wiadomości rozmowy, do
dostawcy wskazanego przez bazowy URL. Klucz jest czytany z wiersza ustawień
dopiero przy budowaniu zapytania i nigdy nie trafia do logu ani komunikatu
błędu. Odmowy przychodzą jako stabilne kody i są pokazywane jako stałe zdania.
Rozmowa żyje w pamięci okna: zamknięcie okna tylko je chowa, a kończy ją
zakończenie aplikacji.

## Keyvault

Aplikacja czyta prywatnie hostowany keyvault przez wspólną tożsamość agenta
urządzenia w `~/.config/keyvault/agent.json` (tryb 600) — jeden plik, czytany
przez każdego konsumenta na maszynie, zawierający adres sejfu, prywatny JWK po
stronie urządzenia, który otwiera to, co sejf zapieczętuje, oraz token na
każdego konsumenta:

```json
{
  "url": "https://<deployment>.convex.site",
  "privateJwk": { "kty": "RSA", "n": "...", "e": "...", "d": "...", "p": "...", "q": "..." },
  "tokens": { "mcp": "kv_...", "clipboard-history": "kv_..." }
}
```

Klucz jest wspólny, bo druga kopia to druga rzecz do rotowania, a kopia, o której
zapomnisz, sama się nie zgłosi — zawiedzie jako błąd odszyfrowania przy każdej
ponownie zapieczętowanej kopercie, wskazując kopertę, choć winny był klucz.
Tokeny *nie* są wspólne z odwrotnego powodu: jeden na konsumenta oznacza, że
jeden można unieważnić bez wyłączania pozostałych. Konsument, którego plik nie
wymienia, jest po prostu nieskonfigurowany. Ustaw `KEYVAULT_AGENT_FILE`, by
trzymać tożsamość gdzie indziej.

Panel ustawień oferuje tylko nadpisania — adres sejfu i token, oba opcjonalne,
by skierować jedną instalację na inne wdrożenie. Celowo nie ma pola na klucz
prywatny: nie należy on do tej aplikacji. Nieskonfigurowany panel nic nie robi,
a aplikacja pozostaje poza siecią.

Sejf zwraca zapieczętowane koperty, a nie wartości: token tylko uwierzytelnia,
a odpowiedź może otworzyć wyłącznie klucz prywatny tego urządzenia. Kopiowanie
sekretu odszyfrowuje go wewnątrz procesu rdzenia i wstawia prosto do schowka —
uzbrajając to samo tłumienie własnych zapisów, którego używa paleta, więc
pobrany klucz nigdy nie trafia do historii — a interfejs dowiaduje się tylko,
czy się udało. Odmowy przychodzą jako kody (brak autoryzacji, poza zakresem,
limit zapytań, dostęp agenta wyłączony) i są pokazywane jako stałe zdania;
odczyty są rozłożone w czasie, by uszanować limit zapytań sejfu na token.

Mówiąc wprost: ta aplikacja nie przechowuje żadnego klucza. Plik tożsamości to
plik z trybem 600 w Twoim katalogu domowym, a jeśli ustawisz nadpisany token,
trafi on do wiersza ustawień — jawnego wiersza w lokalnej bazie, na tej samej
granicy zaufania, na której stoi sama baza. Traktuj przejęcie katalogu danych
jako przejęcie każdego zapisanego tam tokenu i unieważnij go w sejfie; klucza
prywatnego nie ma w bazie, więc nie da się go z niej stracić. Wiersz zapisany
przez starszą wersję, która trzymała tam klucz, jest czyszczony przy pierwszym
zapisie ustawień.

## Prywatność

- Treść schowka, zapytania i ścieżki nigdy nie trafiają do logów. Logi zawierają
  identyfikatory operacji, liczniki, czasy i kody błędów. Dotyczy to także
  adresów pobieranych dla podglądu linków.
- Wartość sekretu z keyvault istnieje tylko między odszyfrowaniem a zapisem do
  schowka: nigdy nie trafia do interfejsu, logu, komunikatu błędu ani historii.
  Token sejfu i klucz prywatny również nigdzie nie są powtarzane.
- Interfejs ani nie otwiera bazy, ani nie czyta dowolnych plików; rozmawia
  z rdzeniem przez wąski zestaw typowanych poleceń.
- Importowany HTML i kod są wyświetlane jako tekst, nigdy jako znaczniki.
- Pliki eksportu i katalog `data/` są ignorowane przez Git i nie mogą trafić do
  repozytorium ani do pakietu aplikacji.

## Struktura

```text
apps/desktop-ui          React + TypeScript + Vite
crates/trove-core    encje, kanonikalizacja, haszowanie
crates/trove-store   SQLite, migracje, CAS, zapisujący
crates/trove-search  normalizacja polska, FTS5, ranking
crates/trove-import  parsery Raycast/SuperCmd, usługa importu
crates/trove-images  miniatury z twardymi limitami
src-tauri                cykl życia, IPC, uprawnienia
tools/trove-import-cli  prywatny import i weryfikacja
tools/trove-bench       syntetyczna historia i pomiary skali
```
