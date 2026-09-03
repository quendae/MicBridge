# MicBridge

Przenosi mikrofon z jednego komputera na drugi po sieci lokalnej. Na maszynie
ze źródłem uruchamiasz `send`, na maszynie docelowej `recv` — i mikrofon
pojawia się tam jako zwykłe urządzenie wejściowe.

Założenia, decyzje projektowe i plan działania: **[docs/ARCHITEKTURA.md](docs/ARCHITEKTURA.md)**
(ta sama treść z diagramami, do czytania w przeglądarce:
[dokument techniczny](https://claude.ai/code/artifact/c6f3e44a-ac5e-4cda-8299-fce46b05237f)).

## Stan: etap M3

Silnik jest gotowy: Opus z FEC, bufor adaptacyjny, korekcja dryfu zegarów,
a od M3 także wirtualne wejście po obu stronach. Brakuje tego, co czyni z tego
produkt — wykrywania w sieci, parowania i okna.

| Etap | Zakres | Stan |
|------|--------|------|
| M0 | weryfikacja łańcucha bez kodu (VBAN) | pominięte — zastąpione `--device tone` |
| M1 | PCM po UDP, wybór urządzeń, bufor jitter, kanał sterujący | **gotowe** |
| M2 | Opus, FEC, bufor adaptacyjny, korekcja dryfu, resampling | **gotowe** |
| M3 | wirtualne wejście: node PipeWire, wykrywanie VB-CABLE | **kod gotowy**, strona linuksowa czeka na test na sprzęcie |
| M4 | mDNS, parowanie SPAKE2, okno egui | |
| M5 | pakiety deb/rpm/AUR/Flatpak/MSI | |

Czego wciąż nie ma, świadomie: szyfrowania i parowania, wykrywania w sieci
(adres wpisuje się ręcznie), okna.

## Budowanie

### Windows

```powershell
winget install Rustlang.Rustup
winget install Microsoft.VisualStudio.2022.BuildTools `
  --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
cargo build --release
```

### Linux

Potrzebne nagłówki ALSA — cpal buduje się na nich niezależnie od tego, że
docelowo pracujemy przez PipeWire:

Nagłówki ALSA są potrzebne, bo cpal linkuje się z nimi niezależnie od tego, że
docelowo pracujemy przez PipeWire. Nagłówki PipeWire i libclang idą do
wirtualnego mikrofonu — `pipewire-rs` generuje wiązania w czasie budowania.

```bash
# Debian / Ubuntu
sudo apt install build-essential pkg-config libasound2-dev libpipewire-0.3-dev libclang-dev
# Fedora
sudo dnf install gcc pkgconf-pkg-config alsa-lib-devel pipewire-devel clang-devel
# Arch
sudo pacman -S base-devel alsa-lib pipewire clang

cargo build --release
```

## Użycie

```bash
# co widać w systemie
micbridge devices

# odbiornik — uruchamiany PIERWSZY, bo to on nasłuchuje
micbridge recv --sink auto --buffer-ms 30

# nadajnik
micbridge send --to 192.168.1.40 --device "yeti"
```

Urządzenie nie musi pracować przy 48 kHz — różnicę zdejmuje resampler po tej
stronie, która jej potrzebuje.

Przydatne flagi:

| Flaga | Do czego |
|-------|----------|
| `send --bitrate 32000` | więcej bitów, gdy 24 kbps nie wystarcza |
| `send --gain-db 6` | cichy mikrofon |
| `send --drop-pct 5` | diagnostyka: gub celowo pakiety i patrz, czy FEC nadąża |
| `recv --fixed-buffer` | trzymaj zadaną poduszkę zamiast dopasowywać ją do łącza |

### Ujście, czyli gdzie ląduje dźwięk

To jedyne miejsce, w którym oba systemy różnią się naprawdę.

| `--sink` | Linux | Windows |
|---|---|---|
| `auto` (domyślne) | tworzy własny mikrofon „MicBridge” w PipeWire | szuka wirtualnego kabla po nazwie |
| `virtual` | to samo, wymuszone | odrzucane przy starcie |
| `device` albo fragment nazwy | zwykłe urządzenie wyjściowe | zwykłe urządzenie wyjściowe |

W Linuksie nie trzeba nic instalować: proces zgłasza się grafowi PipeWire jako
źródło dźwięku i pojawia się na listach mikrofonów pod nazwą „MicBridge”.

W Windows nie da się utworzyć urządzenia wejściowego bez sterownika trybu
jądra z podpisem EV, więc potrzebny jest jednorazowo
[VB-CABLE](https://vb-audio.com/Cable/) — piszemy do `CABLE Input`, aplikacja
wybiera mikrofon `CABLE Output`. Po instalacji warto ustawić w
`VBCABLE_ControlPanel.exe` parametr *Max Latency* na 2048 sampli; domyślne 7168
dokłada około 130 ms, czyli więcej niż cała reszta łańcucha razem wzięta.

### Bez mikrofonu

`--device tone` nadaje sinus 440 Hz o amplitudzie −12 dBFS. Sprawdza całą
ścieżkę — ramkowanie, sieć, bufor, ujście — na maszynie bez mikrofonu i daje
sygnał o znanym kształcie do porównania po drugiej stronie.

```powershell
micbridge recv --sink @0
micbridge send --to 127.0.0.1 --device tone
```

### Porty

TCP 47100 (sterowanie), UDP 47101 (media). Odbiornik musi mieć oba otwarte
w zaporze.

## Test w Linuksie od zera

Poniżej Arch (i pochodne, m.in. Omarchy — PipeWire i Wireplumber są tam
domyślne, więc nic nie trzeba przestawiać). Dla innych rodzin dystrybucji
pakiety są w sekcji [Budowanie](#linux).

> Dopóki [PR #1](https://github.com/quendae/MicBridge/pull/1) nie jest
> wmergowany, wirtualny mikrofon żyje na gałęzi — po sklonowaniu zrób
> `git checkout m3-wirtualne-wejscie`.

### 1. Zależności i budowanie

```bash
sudo pacman -S --needed base-devel alsa-lib pipewire clang git
# Rust, jeśli jeszcze go nie ma:
sudo pacman -S --needed rustup && rustup default stable

git clone https://github.com/quendae/MicBridge.git
cd MicBridge
cargo build --release
```

Arch nie rozdziela pakietów deweloperskich, więc `pipewire` i `alsa-lib` niosą
nagłówki od razu. `clang` jest potrzebny, bo `pipewire-rs` generuje wiązania
w czasie budowania. Pierwsze budowanie kompiluje też libopus ze źródeł i trwa
kilka minut.

```bash
./target/release/micbridge devices
```

Lista przyjdzie przez ALSA, więc wygląda inaczej niż w `pavucontrol` — zobaczysz
pozycje w rodzaju `pipewire`, `default`, `sysdefault` obok kart sprzętowych.
Do wskazania mikrofonu i tak wystarczy fragment nazwy.

### 2. Czy wirtualny mikrofon powstaje

Ujście otwiera się dopiero po nawiązaniu sesji — mikrofon pojawia się w systemie
wtedy, gdy jest co przez niego puścić. Potrzebne są więc dwa terminale.

```bash
# terminal 1 — odbiornik
./target/release/micbridge recv --sink auto

# terminal 2 — nadajnik, syntetyczny ton zamiast mikrofonu
./target/release/micbridge send --to 127.0.0.1 --device tone
```

Odbiornik powinien wypisać `wirtualne wejście utworzone` oraz linijkę
`w aplikacji (Discord, OBS, gra) wybierz mikrofon „MicBridge”`. W trzecim
terminalu:

```bash
wpctl status | grep -i micbridge
# albo
pactl list sources short | grep -i micbridge
```

Od tej chwili „MicBridge” jest na liście mikrofonów w `pavucontrol`, Discordzie
i OBS-ie.

### 3. Czy płynie przez niego dźwięk

```bash
timeout 5 pw-record --target micbridge /tmp/mic.wav
pw-play /tmp/mic.wav
```

Powinien być czysty ton 440 Hz, bez trzasków i przerw. To sprawdza cały
łańcuch: kodowanie, sieć, bufor jitter, resampling i wirtualne wejście.

Odbiornik wypisuje co sekundę stan bufora, straty i korektę dryfu. Przy pętli
lokalnej bufor ma stanąć na zadanych 30 ms, straty na zerze, a korekta dryfu
zejść w okolice zera w kilkanaście sekund.

### 4. Odporność na straty

```bash
./target/release/micbridge send --to 127.0.0.1 --device tone --drop-pct 5
```

Gubi celowo co dwudziesty pakiet. Licznik `FEC` po stronie odbiornika ma rosnąć,
`cel` bufora zostać na 30 ms, a ton w `pw-record` brzmieć dalej gładko — od tego
jest korekcja błędów w Opusie.

### 5. Między maszynami

Na Linuksie:

```bash
./target/release/micbridge recv --sink auto
```

Na Windows (`ip a` po stronie Linuksa poda adres):

```powershell
micbridge.exe send --to 192.168.1.42 --device "yeti"
```

Kierunek odwrotny działa tą samą ścieżką kodu — wtedy w Linuksie uruchamiasz
`send`, a w Windows `recv --sink auto`, i tam potrzebny jest
[VB-CABLE](https://vb-audio.com/Cable/).

Odbiornik musi mieć otwarte **TCP 47100** i **UDP 47101**. Omarchy domyślnie
nie stawia zapory; jeśli ją włączyłeś:

```bash
sudo ufw allow 47100/tcp && sudo ufw allow 47101/udp
```

### Gdy coś nie gra

| Objaw | Co sprawdzić |
|---|---|
| `nie mogę utworzyć wirtualnego mikrofonu` | `systemctl --user status pipewire pipewire-pulse wireplumber` |
| Nie ma „MicBridge” na liście | Czy nadajnik jest połączony — węzeł powstaje dopiero z sesją |
| Trzaski, `NIEDOMIAR` w logu | Podnieś poduszkę: `recv --buffer-ms 60` |
| Bufor rośnie i nie wraca | Zgłoś z logiem `-vv` — to regulator dryfu, nie sieć |
| Nadajnik nie widzi mikrofonu | `micbridge devices`, potem `--device "<fragment nazwy>"` |

Więcej logów: `-v` (debug) albo `-vv` (trace) przy dowolnej komendzie.

## Układ kodu

```
crates/mb-proto/    ramkowanie RTP, sterowanie CBOR, rozszerzanie numeru sekwencji
crates/mb-audio/    enumeracja i strumienie nad WASAPI / ALSA, mono f32
crates/mb-engine/   kodek, bufor jitter, regulator dryfu, resampler, symulator sieci
crates/mb-app/      CLI; okno egui dochodzi w M4
```

Bufor jitter trzyma pakiety **zakodowane**, nie zdekodowany dźwięk. Opus
odtwarza zgubioną ramkę z zapasowej kopii wiezionej w ramce następnej, więc
dekodowanie musi nastąpić po uporządkowaniu, z następnikiem w ręku —
dekodowanie przy odbiorze wyrzucałoby tę możliwość do kosza.

## Jak to jest testowane

`mb-engine` nie dotyka karty dźwiękowej ani gniazda, więc cały rdzeń da się
napędzić syntetycznym strumieniem. `netsim` daje trzy defekty prawdziwej
sieci — stratę, zmienne opóźnienie i wynikające z niego przestawienia — z
ziarnowanego generatora, więc awaria jest powtarzalna, a ośmiogodzinny przebieg
trwa sekundę. `tc netem` zostaje właściwym narzędziem, ale wymaga dwóch maszyn
i Linuksa.

Kryterium wyjścia z M2, spełnione:

* osiem godzin przy dryfie od −50 do +50 ppm — opóźnienie mieści się w 3 ms od
  celu (bez regulatora ten sam dryf dokłada blisko sekundę, co osobny test
  sprawdza, żeby pierwszy nie mógł przejść przypadkiem)
* 2% strat przez 60 s prawdziwego Opusa — ani jednej cichej ramki, straty
  odtwarzane z FEC ponad czterokrotnie częściej niż ukrywane
* 25 ms jittera na odstępie 10 ms, czyli pakiety regularnie się wyprzedzają —
  poniżej 1% dziur

Pętla lokalna na Windows z `--drop-pct 5`: bufor zbiega do 30 ms, FEC odtwarza
straty, koder sam podnosi redundancję do 5%.

## Co wyszło dopiero z uruchomienia

**Bufor osiadał na 200 ms zamiast 30** (M1). Karta dźwiękowa rusza z
opóźnieniem rzędu sekundy, przez ten czas pakiety się piętrzą, a potem tempo
produkcji równa się tempu konsumpcji — nadmiar nigdy sam nie znika. Bufor
zrzuca go przy wychodzeniu z prefillu, zanim cokolwiek zabrzmi; pacer czeka na
pierwsze żądanie karty, żeby to przycięcie wypadło dokładnie na starcie
odtwarzania, a nie przed nim.

**Bufor adaptacyjny uciekał do sufitu** (M2). Podnosiłem cel przy każdej
stracie. To brzmi rozsądnie i jest błędne: poduszka kupuje czas dla pakietu,
który *jeszcze jest w drodze*, a zgubiony nie przyjdzie nigdy — od niego jest
FEC. Przy 5% strat dawało to pięć podniesień na sekundę wobec jednego obniżenia
na trzydzieści spokojnych sekund, których nigdy nie było. Cel podnoszą teraz
wyłącznie pakiety spóźnione i puste przebiegi, z ograniczeniem częstotliwości,
żeby jeden zryw liczył się jako jedno zdarzenie.

**Raportowane `bufor`** obejmuje bufor jitter **i** pierścień przed kartą —
sama głębokość bufora zaniżałaby opóźnienie o dwie ramki. Regulator dryfu
celuje jednak w sam bufor jitter: pierścień to stałe opóźnienie lokalne, nie
zapas na kaprysy sieci.

## Znane zachowania

* Gdy karta rusza szczególnie wolno, poduszka startuje z 60–80 ms i regulator
  ściąga ją do celu przez kilkanaście sekund. To jest wybór: zejście przez
  resampling jest niesłyszalne, wyrzucenie ramek trzaskałoby.
* Korekta dryfu w spoczynku waha się w granicach ±0,1%, bo głębokość bufora
  mierzymy w całych ramkach. To 0,017 półtonu — poniżej progu słyszalności.
* `rubato` jest przypięte do 0.16, choć jest już 5.0; API zmieniło się na tyle,
  że aktualizacja to osobne zadanie.
* Wirtualny mikrofon istnieje tylko w trakcie sesji, bo ujście otwiera się
  dopiero po uzgodnieniu. Aplikacja, która wylistowała mikrofony wcześniej,
  zobaczy „MicBridge” dopiero po odświeżeniu listy. Do rozstrzygnięcia w M4,
  gdy dojdzie okno: wtedy węzeł może istnieć przez cały czas, kiedy odbiór jest
  włączony.

## Licencja

MIT albo Apache-2.0, do wyboru — [LICENSE-MIT](LICENSE-MIT),
[LICENSE-APACHE](LICENSE-APACHE).
