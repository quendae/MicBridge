# MicBridge

Przenosi mikrofon z jednego komputera na drugi po sieci lokalnej. Na maszynie
ze źródłem uruchamiasz `send`, na maszynie docelowej `recv` — i mikrofon
pojawia się tam jako zwykłe urządzenie wejściowe.

Projekt architektoniczny: [dokument techniczny](https://claude.ai/code/artifact/c6f3e44a-ac5e-4cda-8299-fce46b05237f).

## Stan: etap M1

Działa surowy PCM po UDP z ręcznie podanym adresem. To szkielet, nie produkt.

| Etap | Zakres | Stan |
|------|--------|------|
| M0 | weryfikacja łańcucha bez kodu (VBAN) | pominięte — zastąpione generatorem `--device tone` |
| M1 | PCM po UDP, wybór urządzeń, bufor jitter, kanał sterujący | **gotowe** |
| M2 | Opus, FEC, bufor adaptacyjny, korekcja dryfu zegarów | następne |
| M3 | wirtualne wejście: node PipeWire, wykrywanie VB-CABLE | |
| M4 | mDNS, parowanie SPAKE2, okno egui | |
| M5 | pakiety deb/rpm/AUR/Flatpak/MSI | |

Ograniczenia M1, świadome: brak kodeka (768 kbps), brak szyfrowania, brak
konwersji częstotliwości (urządzenie musi pracować przy 48 kHz), bufor
o stałej głębokości, adres wpisywany ręcznie.

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
crates/mb-audio/    enumeracja i strumienie nad WASAPI / ALSA, mono f32 48 kHz
crates/mb-engine/   bufor jitter i statystyki — bez systemu operacyjnego, w pełni testowalne
crates/mb-app/      CLI; okno egui dochodzi w M4
```

`mb-engine` nie dotyka karty dźwiękowej ani gniazda, więc cały rdzeń da się
przetestować syntetycznym strumieniem pakietów.

## Uwagi z testów

Pętla lokalna na Windows, 12 s tonu przez `127.0.0.1`: zero strat, jitter
0,5 ms, bufor stabilnie na 30 ms.

Pierwsza wersja osiadała na 200 ms zamiast 30. Przyczyna: przy starcie karta
dźwiękowa rusza z opóźnieniem, przez ten czas pakiety się piętrzą, a potem
tempo produkcji równa się tempu konsumpcji — nadmiar nigdy sam nie znika.
Bufor zrzuca go teraz przy wychodzeniu z prefillu, zanim cokolwiek zabrzmi
(`JitterBuffer::trimmed`). Powolny dryf zegarów to osobny problem i należy
do M2.

Raportowane `bufor` obejmuje bufor jitter **i** pierścień przed kartą — samo
podanie głębokości bufora zaniżałoby opóźnienie o dwie ramki.
