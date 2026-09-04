//! Teksty programu we wszystkich językach, w jednym miejscu.
//!
//! Język wybiera się sam, z ustawień systemu, i nie da się go zmienić z okna.
//! To celowe: człowiek, który ustawił sobie system po niemiecku, chce widzieć
//! niemiecki wszędzie, a nie szukać w każdym programie osobnego przełącznika.
//! Do testów i do nietypowych przypadków zostaje zmienna `MICBRIDGE_LANG`.
//!
//! Katalog leży w [`catalog`] jako jedna tabela: klucz i po jednym tłumaczeniu
//! na kolumnę. Makro rozwija ją w wyliczenie i w dopasowanie, więc dodanie
//! języka to dopisanie kolumny, a pominięcie jednego tłumaczenia jest błędem
//! kompilacji, nie dziurą, która wychodzi u użytkownika.
//!
//! Nie ma tu tekstów, które czyta wyłącznie programista: komunikatów z dziennika
//! ani błędów opisujących stan protokołu w rodzaju „oczekiwałem HELLO”. Te
//! zostają po polsku, bo trafiają do zgłoszenia błędu, a nie na ekran.

use std::fmt::Display;
use std::sync::OnceLock;

mod catalog;

pub use catalog::Key;

/// Języki, na które program jest przetłumaczony.
///
/// Angielski jest zapasowy: system ustawiony na cokolwiek innego dostanie
/// właśnie jego, bo to najbezpieczniejszy wybór dla kogoś, kogo języka
/// akurat nie znamy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Pl,
    En,
    De,
    Es,
    Fr,
    It,
    Uk,
}

impl Lang {
    /// Rozpoznaje język po znaczniku w rodzaju `pl_PL.UTF-8` albo `de-AT`.
    ///
    /// Liczy się tylko część przed kreską: różnice między odmianami są dla
    /// tych kilkudziesięciu zdań bez znaczenia, a rozpoznanie „pt-BR” jako
    /// portugalskiego jest lepsze niż nierozpoznanie go wcale.
    pub fn from_tag(tag: &str) -> Option<Self> {
        let base = tag
            .split(['-', '_', '.', '@'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        Some(match base.as_str() {
            "pl" => Self::Pl,
            "en" => Self::En,
            "de" => Self::De,
            "es" => Self::Es,
            "fr" => Self::Fr,
            "it" => Self::It,
            "uk" => Self::Uk,
            _ => return None,
        })
    }

    /// Co mówi system. Zmienna programu ma pierwszeństwo nad wszystkim.
    pub fn detect() -> Self {
        if let Some(lang) = std::env::var("MICBRIDGE_LANG")
            .ok()
            .and_then(|v| Self::from_tag(&v))
        {
            return lang;
        }
        Self::from_system().unwrap_or(Self::En)
    }

    #[cfg(windows)]
    fn from_system() -> Option<Self> {
        use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;

        // Windows oddaje znacznik w rodzaju „pl-PL” jako ciąg dwubajtowy;
        // stała LOCALE_NAME_MAX_LENGTH to 85 znaków.
        let mut buf = [0u16; 85];
        // SAFETY: dajemy bufor i jego prawdziwą długość, a funkcja pisze
        // wyłącznie w jego granicach.
        let len = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
        if len <= 1 {
            return None;
        }
        // Zwrócona długość obejmuje zamykające zero.
        let tag = String::from_utf16_lossy(&buf[..(len - 1) as usize]);
        Self::from_tag(&tag)
    }

    #[cfg(not(windows))]
    fn from_system() -> Option<Self> {
        // Kolejność jak w POSIX: LC_ALL przykrywa wszystko, LC_MESSAGES dotyczy
        // wprost komunikatów, LANG jest ustawieniem ogólnym.
        ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .filter_map(|name| std::env::var(name).ok())
            .find(|v| !v.is_empty() && v != "C" && v != "POSIX")
            .and_then(|v| Self::from_tag(&v))
    }
}

/// Język tej sesji. Rozpoznany raz, przy pierwszym pytaniu.
///
/// Leniwie, bo teksty bywają potrzebne, zanim program zdąży cokolwiek ustawić —
/// wiersz poleceń wypisuje pomoc jeszcze przed wejściem do `main`.
pub fn lang() -> Lang {
    static LANG: OnceLock<Lang> = OnceLock::new();
    *LANG.get_or_init(Lang::detect)
}

/// Tekst bez miejsc do wypełnienia.
pub fn t(key: Key) -> &'static str {
    catalog::text(key, lang())
}

/// Tekst z jednym miejscem do wypełnienia, zapisanym jako `{}`.
pub fn t1(key: Key, a: impl Display) -> String {
    fill(t(key), &[&a.to_string()])
}

pub fn t2(key: Key, a: impl Display, b: impl Display) -> String {
    fill(t(key), &[&a.to_string(), &b.to_string()])
}

pub fn t3(key: Key, a: impl Display, b: impl Display, c: impl Display) -> String {
    fill(t(key), &[&a.to_string(), &b.to_string(), &c.to_string()])
}

/// Podmienia kolejne `{}` na kolejne wartości.
///
/// Własne zamiast `format!`, bo tamto wymaga wzorca znanego w czasie
/// kompilacji, a nasz przychodzi z tabeli wybieranej w czasie działania.
fn fill(pattern: &str, values: &[&str]) -> String {
    let mut out = String::with_capacity(pattern.len() + 16);
    let mut rest = pattern;
    for value in values {
        match rest.split_once("{}") {
            Some((before, after)) => {
                out.push_str(before);
                out.push_str(value);
                rest = after;
            }
            // Więcej wartości niż miejsc: tłumaczenie widocznie przestawiło
            // zdanie. Dokładamy na końcu, żeby liczba nie zniknęła.
            None => {
                out.push_str(rest);
                rest = "";
                out.push(' ');
                out.push_str(value);
            }
        }
    }
    out.push_str(rest);
    out
}

/// Liczebnik z rzeczownikiem, we właściwej formie dla języka.
///
/// „1 urządzeń” w programie, który poza tym mówi po ludzku, wygląda jak
/// niedokończona robota — a w polskim i ukraińskim form jest trzy, nie dwie.
pub fn devices(n: usize) -> String {
    devices_in(lang(), n)
}

fn devices_in(lang: Lang, n: usize) -> String {
    let key = match lang {
        Lang::Pl | Lang::Uk => match (n % 10, n % 100) {
            (1, 11) => Key::DevicesMany,
            (1, _) => Key::DevicesOne,
            (2..=4, 12..=14) => Key::DevicesMany,
            (2..=4, _) => Key::DevicesFew,
            _ => Key::DevicesMany,
        },
        // Reszta rozróżnia tylko jeden i nie-jeden.
        _ => {
            if n == 1 {
                Key::DevicesOne
            } else {
                Key::DevicesMany
            }
        }
    };
    format!("{n} {}", catalog::text(key, lang))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_boil_down_to_the_language() {
        assert_eq!(Lang::from_tag("pl_PL.UTF-8"), Some(Lang::Pl));
        assert_eq!(Lang::from_tag("de-AT"), Some(Lang::De));
        assert_eq!(Lang::from_tag("EN"), Some(Lang::En));
        assert_eq!(Lang::from_tag("uk_UA"), Some(Lang::Uk));
        assert_eq!(Lang::from_tag("ja_JP"), None, "nieznany zostaje nieznany");
        assert_eq!(Lang::from_tag(""), None);
    }

    #[test]
    fn filling_walks_through_the_holes_in_order() {
        assert_eq!(fill("{} z {} cyfr", &["4", "6"]), "4 z 6 cyfr");
        assert_eq!(fill("bez miejsc", &[]), "bez miejsc");
    }

    #[test]
    fn a_value_with_nowhere_to_go_lands_at_the_end() {
        // Tłumaczenie może przestawić zdanie i zgubić miejsce. Liczba jest
        // wtedy ważniejsza niż uroda zdania.
        assert_eq!(fill("bez miejsca", &["7"]), "bez miejsca 7");
    }

    #[test]
    fn every_key_has_every_translation() {
        // Sam fakt, że to się kompiluje, gwarantuje komplet — makro wymaga
        // wszystkich kolumn. Tu sprawdzamy, że żadna nie została pusta.
        for key in catalog::ALL {
            for lang in [
                Lang::Pl,
                Lang::En,
                Lang::De,
                Lang::Es,
                Lang::Fr,
                Lang::It,
                Lang::Uk,
            ] {
                assert!(
                    !catalog::text(*key, lang).is_empty(),
                    "puste tłumaczenie: {key:?} w {lang:?}"
                );
            }
        }
    }

    #[test]
    fn slavic_numerals_take_the_right_form() {
        // „1 urządzeń” wygląda jak niedokończona robota, a nastolatki są
        // wyjątkiem: dwanaście urządzeń, nie dwanaście urządzenia.
        assert_eq!(devices_in(Lang::Pl, 1), "1 urządzenie");
        assert_eq!(devices_in(Lang::Pl, 2), "2 urządzenia");
        assert_eq!(devices_in(Lang::Pl, 5), "5 urządzeń");
        assert_eq!(devices_in(Lang::Pl, 12), "12 urządzeń");
        assert_eq!(devices_in(Lang::Pl, 22), "22 urządzenia");
        assert_eq!(devices_in(Lang::Pl, 0), "0 urządzeń");
        assert_eq!(devices_in(Lang::Uk, 21), "21 пристрій");
    }

    #[test]
    fn the_rest_only_tell_one_from_many() {
        assert_eq!(devices_in(Lang::En, 1), "1 device");
        assert_eq!(devices_in(Lang::En, 2), "2 devices");
        assert_eq!(devices_in(Lang::De, 0), "0 Geräte");
    }

    #[test]
    fn holes_match_across_languages() {
        // Wzorzec z jednym `{}` po polsku musi mieć jedno `{}` wszędzie —
        // inaczej liczba wyląduje w innym miejscu albo doklei się na końcu.
        for key in catalog::ALL {
            let wanted = catalog::text(*key, Lang::Pl).matches("{}").count();
            for lang in [Lang::En, Lang::De, Lang::Es, Lang::Fr, Lang::It, Lang::Uk] {
                assert_eq!(
                    catalog::text(*key, lang).matches("{}").count(),
                    wanted,
                    "inna liczba miejsc: {key:?} w {lang:?}"
                );
            }
        }
    }
}
