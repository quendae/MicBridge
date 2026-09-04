"""Generuje ikonę programu w trzech postaciach z jednego opisu kształtu.

Ten sam mikrofon na okrągłym tle, co ikona w zasobniku — tam rysowany w kodzie,
tu zapisany do plików, bo instalatory i menu aplikacji potrzebują plików.

PNG i ICO składamy ręcznie zamiast biblioteką: to kilkadziesiąt linii, a nie
chcę zależności budowanej tylko po to, by raz wyprodukować dwa pliki.
"""
import binascii
import io
import os
import struct
import zlib

SIZE = 256
OUT = 'packaging/icons'


def shape(x, y, size):
    """Zwraca kolor RGBA piksela. Skala liczona względem 32-pikselowego wzoru."""
    k = size / 32.0
    centre = (size - 1) / 2.0
    radius = centre - 0.5 * k
    dx = x - centre
    dy = y - centre
    inside = dx * dx + dy * dy <= radius * radius

    # Kapsuła to odcinek pogrubiony o promień — inaczej wychodzi prostokąt,
    # a prostokąt na nóżce wygląda jak kielich, nie jak mikrofon.
    top, bottom, r = -5.5 * k, -2.0 * k, 3.5 * k
    near = dy < top and dx * dx + (dy - top) ** 2 <= r * r
    far = dy > bottom and dx * dx + (dy - bottom) ** 2 <= r * r
    middle = top <= dy <= bottom and abs(dx) <= r
    capsule = near or far or middle

    stem = abs(dx) <= 1.0 * k and 1.5 * k <= dy <= 6.5 * k
    base = 6.0 * k < dy <= 8.0 * k and abs(dx) <= 5.0 * k
    mic = capsule or stem or base

    if mic:
        return (245, 245, 250, 255)
    if inside:
        return (40, 90, 150, 255)
    return (0, 0, 0, 0)


def png_bytes(size):
    raw = bytearray()
    for y in range(size):
        raw.append(0)  # filtr „none” dla wiersza
        for x in range(size):
            raw.extend(shape(x, y, size))

    def chunk(tag, data):
        out = struct.pack('>I', len(data)) + tag + data
        return out + struct.pack('>I', binascii.crc32(tag + data) & 0xFFFFFFFF)

    header = struct.pack('>IIBBBBB', size, size, 8, 6, 0, 0, 0)
    return (b'\x89PNG\r\n\x1a\n'
            + chunk(b'IHDR', header)
            + chunk(b'IDAT', zlib.compress(bytes(raw), 9))
            + chunk(b'IEND', b''))


def ico_bytes(sizes):
    """ICO z PNG w środku — dozwolone od Windows Vista i o wiele prostsze
    niż zapis w starym formacie BMP z maską przezroczystości."""
    images = [png_bytes(s) for s in sizes]
    header = struct.pack('<HHH', 0, 1, len(images))
    offset = len(header) + 16 * len(images)
    entries = b''
    for size, data in zip(sizes, images):
        # 0 w polu wymiaru znaczy 256 — pole ma jeden bajt.
        entries += struct.pack('<BBBBHHII', size % 256, size % 256, 0, 0, 1, 32,
                               len(data), offset)
        offset += len(data)
    return header + entries + b''.join(images)


def svg_text():
    """Wektor do menu aplikacji: skaluje się na każdy rozmiar ikony."""
    return '''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32" width="32" height="32">
  <title>MicBridge</title>
  <circle cx="15.5" cy="15.5" r="15" fill="#285a96"/>
  <g fill="#f5f5fa">
    <rect x="12" y="6.5" width="7" height="10" rx="3.5"/>
    <rect x="14.5" y="17" width="2" height="5"/>
    <rect x="10.5" y="21.5" width="10" height="2" rx="1"/>
  </g>
</svg>
'''


os.makedirs(OUT, exist_ok=True)
with open(f'{OUT}/micbridge.png', 'wb') as f:
    f.write(png_bytes(SIZE))
with open(f'{OUT}/micbridge.ico', 'wb') as f:
    f.write(ico_bytes([16, 32, 48, 256]))
with io.open(f'{OUT}/micbridge.svg', 'w', encoding='utf-8', newline='\n') as f:
    f.write(svg_text())

for name in ('micbridge.png', 'micbridge.ico', 'micbridge.svg'):
    print(name, os.path.getsize(f'{OUT}/{name}'), 'B')
