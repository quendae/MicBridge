"""Składa licencję w RTF — instalator MSI nie umie czytać zwykłego tekstu.

Treść bierzemy z plików licencji w repozytorium, żeby nie było dwóch wersji
tego samego dokumentu, które mogą się rozjechać.
"""
import io

HEADER = (r'{\rtf1\ansi\ansicpg1250\deff0'
          r'{\fonttbl{\f0\fnil\fcharset238 Segoe UI;}}'
          '\n' r'\viewkind4\uc1\pard\f0\fs18' '\n')


def escape(text):
    out = []
    for ch in text:
        if ch in '\\{}':
            out.append('\\' + ch)
        elif ord(ch) < 128:
            out.append(ch)
        else:
            # RTF liczy znaki spoza ASCII jako liczby ze znakiem.
            code = ord(ch)
            out.append(r'\u%d?' % (code if code < 32768 else code - 65536))
    return ''.join(out)


def block(path, title):
    with io.open(path, encoding='utf-8') as f:
        body = f.read()
    out = [r'\b ' + escape(title) + r'\b0\par\par']
    for line in body.splitlines():
        out.append(escape(line) + r'\par')
    out.append(r'\par')
    return '\n'.join(out)


parts = [HEADER,
         block('LICENSE-MIT', 'Licencja MIT'),
         block('LICENSE-APACHE', 'Licencja Apache 2.0'),
         '}\n']

with io.open('packaging/windows/license.rtf', 'w', encoding='cp1250',
             errors='replace', newline='\r\n') as f:
    f.write(''.join(parts))

import os
print('license.rtf', os.path.getsize('packaging/windows/license.rtf'), 'B')
