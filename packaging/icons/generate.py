"""Rysuje znak MicBridge i zapisuje go we wszystkich postaciach, jakich
wymagają systemy: PNG i SVG do menu aplikacji, ICO do Windows, surowe RGBA
do ikony w zasobniku i w pasku okna.

Wszystko powstaje z jednego opisu kształtu niżej. Dlatego geometria jest
liczbami, a nie ścieżkami przepisanymi ręcznie: ścieżki SVG też składamy z tych
samych liczb, więc poprawka w jednym miejscu przechodzi na wszystkie pliki i nic
się nie rozjeżdża.

PNG, ICO i RGBA składamy bez bibliotek. To kilkadziesiąt linii, a zależność
wnoszona po to, żeby raz na rok wyprodukować pięć plików, kosztuje więcej niż
jest warta.

Uruchamiać z katalogu głównego repozytorium:

    python packaging/icons/generate.py
"""
import binascii
import io
import math
import os
import struct
import zlib

OUT = 'packaging/icons'
NL = '\n'

# Wszystkie wymiary w kwadracie 32×32 — tym samym, w którym pracuje viewBox
# SVG. Skala do pikseli to zwykłe mnożenie.
BOX = 32.0

GRANAT = (11, 32, 80)
KARTA = (245, 248, 252)
# Strzałka przechodzi od morskiego przy ogonie do niebieskiego przy grocie.
OGON = (23, 182, 198)
GROT = (11, 143, 227)

# Mikrofon: kapsuła z przerwą, kabłąk, nóżka, podstawka.
MIC_X = 12.6
KAPSULA_R = 3.1
KAPSULA_GORA = 6.0
KAPSULA_DOL = 16.0
PRZERWA = (9.3, 10.4)

KABLAK_Y = 14.2
KABLAK_ZEWN = 5.6
KABLAK_WEWN = 4.3
# Ramiona kabłąka kończą się nieco powyżej jego środka — dzięki temu obejmuje
# kapsułę, zamiast wisieć pod nią jak miska.
KABLAK_RAMIE = -1.0

NOZKA = (0.95, 19.8, 24.2)  # połowa szerokości, góra, dół
PODSTAWKA = (5.0, 23.6, 25.4, 0.9)  # połowa szerokości, góra, dół, promień

# Strzałki: (początek, koniec). Ogony chowają się w korpusie mikrofonu.
STRZALKI = [((11.5, 13.4), (28.6, 8.4)), ((11.5, 17.2), (29.0, 12.4))]
# Wersja na małe rozmiary. Dwie strzałki poniżej 48 pikseli zlewają się w plamę,
# więc tam zostaje jedna — tak samo, jak robi się z każdą ikoną.
STRZALKA_MALA = [((11.5, 15.0), (28.0, 10.2))]
GRUBOSC = 2.4
GROT_SZER = 3.3
GROT_DL = 4.6

KARTA_R = 7.2  # zaokrąglenie rogów kwadratu pod ikoną programu

# Znak jest niesymetryczny: strzałki wychodzą tylko w prawo, więc mikrofon sam
# z siebie siedzi za bardzo z lewej. Zamiast poprawiać każdą współrzędną
# osobno, całość przenosimy i powiększamy jedną przemianą — środek treści ląduje
# na środku kwadratu, a margines schodzi do jednej dziesiątej.
ZNAK_SRODEK = (18.0, 15.7)
ZNAK_SKALA = 1.15
# Przy szesnastu pikselach margines to już cała kreska rysunku, więc tam znak
# rozpychamy niemal na styk. Poniżej pewnego rozmiaru czytelność wygrywa
# z oddechem wokół.
ZNAK_SKALA_MALA = 1.34


def odcinek(px, py, ax, ay, bx, by):
    """Odległość punktu od odcinka."""
    vx, vy = bx - ax, by - ay
    dlugosc = vx * vx + vy * vy
    t = 0.0 if dlugosc == 0 else ((px - ax) * vx + (py - ay) * vy) / dlugosc
    t = min(1.0, max(0.0, t))
    return math.hypot(px - ax - t * vx, py - ay - t * vy), t


def wielokat(punkty):
    """Zamyka listę wierzchołków w test przynależności (rzucanie promienia)."""

    def wewnatrz(px, py):
        licznik = False
        n = len(punkty)
        for i in range(n):
            ax, ay = punkty[i]
            bx, by = punkty[(i + 1) % n]
            if (ay > py) != (by > py):
                if px < ax + (py - ay) * (bx - ax) / (by - ay):
                    licznik = not licznik
        return licznik

    return wewnatrz


def obrys_strzalki(start, koniec):
    """Siedem wierzchołków: trzon o stałej grubości zakończony grotem."""
    (ax, ay), (bx, by) = start, koniec
    dlugosc = math.hypot(bx - ax, by - ay)
    dx, dy = (bx - ax) / dlugosc, (by - ay) / dlugosc
    px, py = -dy, dx  # prostopadła
    nasada = (bx - dx * GROT_DL, by - dy * GROT_DL)
    t = GRUBOSC / 2.0

    def przesun(punkt, ile):
        return (punkt[0] + px * ile, punkt[1] + py * ile)

    return [
        przesun(start, t),
        przesun(nasada, t),
        przesun(nasada, GROT_SZER),
        (bx, by),
        przesun(nasada, -GROT_SZER),
        przesun(nasada, -t),
        przesun(start, -t),
    ]


def barwa_strzalki(t):
    """Morski przy ogonie, niebieski przy grocie."""
    return tuple(round(a + (b - a) * t) for a, b in zip(OGON, GROT))


def karta_tutaj(x, y):
    """Zaokrąglony kwadrat pod ikoną programu. Liczony w układzie kwadratu,
    nie znaku — tło nie jeździ razem z treścią."""
    wx = max(abs(x - BOX / 2) - (BOX / 2 - KARTA_R), 0.0)
    wy = max(abs(y - BOX / 2) - (BOX / 2 - KARTA_R), 0.0)
    return math.hypot(wx, wy) <= KARTA_R


def warstwy(strzalki):
    """Buduje listę testów „czy tu jest ten kolor”, od spodu do wierzchu."""
    lista = []

    def mikrofon(x, y):
        d, _ = odcinek(x, y, MIC_X, KAPSULA_GORA + KAPSULA_R,
                       MIC_X, KAPSULA_DOL - KAPSULA_R)
        if d <= KAPSULA_R and not (PRZERWA[0] < y < PRZERWA[1]):
            return GRANAT

        promien = math.hypot(x - MIC_X, y - KABLAK_Y)
        if KABLAK_WEWN <= promien <= KABLAK_ZEWN and y - KABLAK_Y >= KABLAK_RAMIE:
            return GRANAT

        pol, gora, dol = NOZKA
        if abs(x - MIC_X) <= pol and gora <= y <= dol:
            return GRANAT

        pol, gora, dol, r = PODSTAWKA
        wx = max(abs(x - MIC_X) - (pol - r), 0.0)
        wy = max(abs(y - (gora + dol) / 2) - ((dol - gora) / 2 - r), 0.0)
        if abs(x - MIC_X) <= pol and gora <= y <= dol and math.hypot(wx, wy) <= r:
            return GRANAT
        return None

    lista.append(mikrofon)

    for start, koniec in strzalki:
        test = wielokat(obrys_strzalki(start, koniec))

        def strzalka(x, y, _test=test, _a=start, _b=koniec):
            if not _test(x, y):
                return None
            _, t = odcinek(x, y, _a[0], _a[1], _b[0], _b[1])
            return barwa_strzalki(t)

        lista.append(strzalka)

    return lista


def piksele(rozmiar, karta):
    """Rasteryzuje z nadpróbkowaniem 4×4 — bez tego skosy strzałek są schodkami."""
    strzalki = STRZALKI if rozmiar >= 48 else STRZALKA_MALA
    lista = warstwy(strzalki)
    skala = ZNAK_SKALA if rozmiar >= 24 else ZNAK_SKALA_MALA
    krok = BOX / rozmiar
    podpiksele = [(i + 0.5) / 4.0 for i in range(4)]
    razem = len(podpiksele) ** 2

    raw = bytearray()
    for py in range(rozmiar):
        raw.append(0)  # filtr „none” dla wiersza PNG
        for px in range(rozmiar):
            r = g = b = a = 0
            for uy in podpiksele:
                y = (py + uy) * krok
                for ux in podpiksele:
                    x = (px + ux) * krok
                    kolor = KARTA if karta and karta_tutaj(x, y) else None
                    zx = (x - BOX / 2) / skala + ZNAK_SRODEK[0]
                    zy = (y - BOX / 2) / skala + ZNAK_SRODEK[1]
                    for warstwa in lista:
                        nowy = warstwa(zx, zy)
                        if nowy is not None:
                            kolor = nowy
                    if kolor is not None:
                        r += kolor[0]
                        g += kolor[1]
                        b += kolor[2]
                        a += 255
            if a == 0:
                raw.extend((0, 0, 0, 0))
            else:
                # Uśredniamy z pomnożoną krytością, więc brzeg nie rozjaśnia się
                # kolorem tła, którego tam nie ma.
                krycie = a // razem
                raw.extend((round(r / (a / 255.0)), round(g / (a / 255.0)),
                            round(b / (a / 255.0)), krycie))
    return bytes(raw)


def rgba(rozmiar, karta):
    """To samo bez bajtu filtra — surowy bufor dla ikony w kodzie."""
    dane = piksele(rozmiar, karta)
    szerokosc = rozmiar * 4 + 1
    out = bytearray()
    for y in range(rozmiar):
        out.extend(dane[y * szerokosc + 1:(y + 1) * szerokosc])
    return bytes(out)


def png_bytes(rozmiar, karta):
    def kawalek(tag, dane):
        out = struct.pack('>I', len(dane)) + tag + dane
        return out + struct.pack('>I', binascii.crc32(tag + dane) & 0xFFFFFFFF)

    naglowek = struct.pack('>IIBBBBB', rozmiar, rozmiar, 8, 6, 0, 0, 0)
    return (b'\x89PNG\r\n\x1a\n'
            + kawalek(b'IHDR', naglowek)
            + kawalek(b'IDAT', zlib.compress(piksele(rozmiar, karta), 9))
            + kawalek(b'IEND', b''))


def ico_bytes(rozmiary):
    """ICO z PNG w środku — dozwolone od Visty i o wiele prostsze niż stary
    zapis BMP z osobną maską przezroczystości."""
    obrazy = [png_bytes(s, True) for s in rozmiary]
    naglowek = struct.pack('<HHH', 0, 1, len(obrazy))
    offset = len(naglowek) + 16 * len(obrazy)
    wpisy = b''
    for rozmiar, dane in zip(rozmiary, obrazy):
        # Zero w polu wymiaru znaczy 256 — pole ma jeden bajt.
        wpisy += struct.pack('<BBBBHHII', rozmiar % 256, rozmiar % 256, 0, 0,
                             1, 32, len(dane), offset)
        offset += len(dane)
    return naglowek + wpisy + b''.join(obrazy)


def hex_kolor(kolor):
    return '#%02x%02x%02x' % kolor


def svg_text(karta, strzalki):
    """Ten sam kształt wektorowo — menu aplikacji skalują ikonę do swoich
    rozmiarów i rastrowa by się w nich rozmyła."""
    n = lambda v: f'{v:.3f}'.rstrip('0').rstrip('.')
    czesci = []

    lewo, prawo = MIC_X - KAPSULA_R, MIC_X + KAPSULA_R
    gora, dol = KAPSULA_GORA + KAPSULA_R, KAPSULA_DOL - KAPSULA_R
    r = KAPSULA_R
    czesci.append(
        f'    <path d="M{n(lewo)},{n(gora)} A{n(r)},{n(r)} 0 0 1 {n(prawo)},{n(gora)} '
        f'L{n(prawo)},{n(PRZERWA[0])} L{n(lewo)},{n(PRZERWA[0])} Z"/>')
    czesci.append(
        f'    <path d="M{n(lewo)},{n(PRZERWA[1])} L{n(prawo)},{n(PRZERWA[1])} '
        f'L{n(prawo)},{n(dol)} A{n(r)},{n(r)} 0 0 1 {n(lewo)},{n(dol)} Z"/>')

    y = KABLAK_Y + KABLAK_RAMIE
    zx = math.sqrt(KABLAK_ZEWN ** 2 - KABLAK_RAMIE ** 2)
    wx = math.sqrt(KABLAK_WEWN ** 2 - KABLAK_RAMIE ** 2)
    z, w = KABLAK_ZEWN, KABLAK_WEWN
    czesci.append(
        f'    <path d="M{n(MIC_X - zx)},{n(y)} A{n(z)},{n(z)} 0 1 0 {n(MIC_X + zx)},{n(y)} '
        f'L{n(MIC_X + wx)},{n(y)} A{n(w)},{n(w)} 0 1 1 {n(MIC_X - wx)},{n(y)} Z"/>')

    pol, gy, dy = NOZKA
    czesci.append(f'    <rect x="{n(MIC_X - pol)}" y="{n(gy)}" '
                  f'width="{n(pol * 2)}" height="{n(dy - gy)}"/>')
    pol, gy, dy, pr = PODSTAWKA
    czesci.append(f'    <rect x="{n(MIC_X - pol)}" y="{n(gy)}" width="{n(pol * 2)}" '
                  f'height="{n(dy - gy)}" rx="{n(pr)}"/>')

    mikrofon = (f'    <g fill="{hex_kolor(GRANAT)}">' + NL
                + NL.join('  ' + c for c in czesci) + NL + '    </g>')

    gradienty, ksztalty = [], []
    for i, (start, koniec) in enumerate(strzalki):
        gradienty.append(
            f'    <linearGradient id="s{i}" gradientUnits="userSpaceOnUse" '
            f'x1="{n(start[0])}" y1="{n(start[1])}" x2="{n(koniec[0])}" y2="{n(koniec[1])}">\n'
            f'      <stop offset="0" stop-color="{hex_kolor(OGON)}"/>\n'
            f'      <stop offset="1" stop-color="{hex_kolor(GROT)}"/>\n'
            f'    </linearGradient>')
        punkty = ' '.join(f'{n(x)},{n(y)}' for x, y in obrys_strzalki(start, koniec))
        ksztalty.append(f'    <polygon points="{punkty}" fill="url(#s{i})"/>')

    tlo = (f'  <rect width="32" height="32" rx="{n(KARTA_R)}" '
           f'fill="{hex_kolor(KARTA)}"/>' + NL) if karta else ''
    # Gradienty opisane są we współrzędnych znaku, a nie kwadratu, bo odnoszą
    # się do układu tego elementu, który po nie sięga — czyli wnętrza grupy.
    skala = ZNAK_SKALA
    przemiana = (f'translate({n(BOX / 2 - ZNAK_SRODEK[0] * skala)},'
                 f'{n(BOX / 2 - ZNAK_SRODEK[1] * skala)}) scale({n(skala)})')

    return ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32" '
            'width="32" height="32">' + NL
            + '  <title>MicBridge</title>' + NL
            + '  <defs>' + NL + NL.join(gradienty) + NL + '  </defs>' + NL
            + tlo
            + f'  <g transform="{przemiana}">' + NL
            + mikrofon + NL
            + NL.join(ksztalty) + NL
            + '  </g>' + NL
            + '</svg>' + NL)


def zapisz(nazwa, dane):
    tryb, kodowanie = ('wb', None) if isinstance(dane, bytes) else ('w', 'utf-8')
    with io.open(f'{OUT}/{nazwa}', tryb, encoding=kodowanie,
                 newline=None if kodowanie is None else '\n') as f:
        f.write(dane)
    print(f'{nazwa:24} {os.path.getsize(f"{OUT}/{nazwa}"):>8} B')


os.makedirs(OUT, exist_ok=True)

# Ikona programu: znak na jasnej karcie, jak w menu i na pulpicie.
zapisz('micbridge.png', png_bytes(256, True))
zapisz('micbridge.ico', ico_bytes([16, 32, 48, 256]))
zapisz('micbridge.svg', svg_text(True, STRZALKI))

# Logo: sam znak, bez tła — do dokumentacji i wszędzie tam, gdzie karta
# wyglądałaby jak naklejka na cudzym tle.
zapisz('logo.png', png_bytes(512, False))
zapisz('logo.svg', svg_text(False, STRZALKI))

# Surowe RGBA dla ikon rysowanych przez sam program: zasobnik i pasek okna.
# Wczytanie gotowego bufora jest tańsze niż wniesienie dekodera PNG tylko po to,
# żeby przy starcie rozpakować dwa obrazki.
zapisz('tray-32.rgba', rgba(32, True))
zapisz('window-64.rgba', rgba(64, True))
