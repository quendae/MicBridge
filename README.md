# MicBridge

Przenosi mikrofon z jednego komputera na drugi po sieci lokalnej. Na maszynie
ze źródłem uruchamiasz `send`, na maszynie docelowej `recv` — i mikrofon
pojawia się tam jako zwykłe urządzenie wejściowe.

Projekt architektoniczny: [dokument techniczny](https://claude.ai/code/artifact/c6f3e44a-ac5e-4cda-8299-fce46b05237f).

## Stan: etap M2

Działa Opus z FEC, bufor adaptacyjny i korekcja dryfu zegarów. Silnik jest
gotowy; brakuje wszystkiego, co czyni z tego produkt.

| Etap | Zakres | Stan |
|------|--------|------|
| M0 | weryfikacja łańcucha bez kodu (VBAN) | pominięte — zastąpione `--device tone` |
| M1 | PCM po UDP, wybór urządzeń, bufor jitter, kanał sterujący | **gotowe** |
| M2 | Opus, FEC, bufor adaptacyjny, korekcja dryfu, resampling | **gotowe** |
| M3 | wirtualne wejście: node PipeWire, wykrywanie VB-CABLE | następne |
| M4 | mDNS, parowanie SPAKE2, okno egui | |
| M5 | pakiety deb/rpm/AUR/Flatpak/MSI | |

Czego wciąż nie ma, świadomie: szyfrowania i parowania, wykrywania w sieci
(adres wpisuje się ręcznie), tworzenia wirtualnego wejścia w Linuksie, okna.

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

```bash
# Debian / Ubuntu
sudo apt install build-essential pkg-config libasound2-dev
# Fedora
sudo dnf install gcc pkgconf-pkg-config alsa-lib-devel
# Arch
sudo pacman -S base-devel alsa-lib

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

`--sink auto` szuka wirtualnego kabla po nazwie (VB-CABLE, VoiceMeeter, VAC).
Póki M3 nie doda tworzenia węzła PipeWire, na Linuksie wskaż ujście ręcznie.

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
