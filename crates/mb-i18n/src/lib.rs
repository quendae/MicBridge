//! Teksty programu we wszystkich językach, w jednym miejscu.
//!
//! Język wybiera się sam, z ustawień systemu — bo kto ustawił sobie system po
//! niemiecku, chce niemieckiego wszędzie, a nie osobnego przełącznika w każdym
//! programie. Automat bywa jednak w błędzie: system po angielsku u kogoś, kto
//! woli polski, albo pulpit, który nie mówi programom prawdy. Dlatego wybór da
//! się nadpisać z okna, a program go pamięta — obok kluczy parowania, w tym
//! samym katalogu, w pliku `language`.
//!
//! Kolejność jest taka: zmienna `MICBRIDGE_LANG`, potem zapisany wybór, potem
//! system, a na końcu angielski. Zmienna stoi najwyżej, bo jest do uruchomienia
//! programu w obcym języku na chwilę, bez ruszania tego, co zapisane.
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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};

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

/// Wszystkie języki w kolejności, w jakiej mają stać na liście.
pub const LANGS: [Lang; 7] = [
    Lang::Pl,
    Lang::En,
    Lang::De,
    Lang::Es,
    Lang::Fr,
    Lang::It,
    Lang::Uk,
];

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

    /// Co mówi system, a gdy nic sensownego nie mówi — angielski.
    pub fn automatic() -> Self {
        Self::from_system().unwrap_or(Self::En)
    }

    /// Dwuliterowy zapis. To trafia do pliku z wyborem.
    pub fn code(self) -> &'static str {
        match self {
            Self::Pl => "pl",
            Self::En => "en",
            Self::De => "de",
            Self::Es => "es",
            Self::Fr => "fr",
            Self::It => "it",
            Self::Uk => "uk",
        }
    }

    /// Nazwa języka w nim samym.
    ///
    /// Nie ma jej w tabeli tłumaczeń i nie powinno być: „Deutsch” nazywa się
    /// tak samo na polskim ekranie i na hiszpańskim. Kto szuka swojego języka
    /// na liście, szuka właśnie tego zapisu — a nie obcego słowa na siebie.
    pub fn endonym(self) -> &'static str {
        match self {
            Self::Pl => "Polski",
            Self::En => "English",
            Self::De => "Deutsch",
            Self::Es => "Español",
            Self::Fr => "Français",
            Self::It => "Italiano",
            Self::Uk => "Українська",
        }
    }

    fn index(self) -> u8 {
        LANGS.iter().position(|l| *l == self).unwrap_or(1) as u8
    }

    fn from_index(index: u8) -> Option<Self> {
        LANGS.get(index as usize).copied()
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

/// „Jeszcze nie sprawdzone” w licznikach niżej.
const UNSET: u8 = u8::MAX;
/// „Niech program wybiera sam”.
const AUTO: u8 = u8::MAX - 1;

/// Język, którym program mówi w tej chwili.
static CURRENT: AtomicU8 = AtomicU8::new(UNSET);
/// Co wybrał człowiek: język albo [`AUTO`].
static PICKED: AtomicU8 = AtomicU8::new(UNSET);

/// Ustala język, jeśli jeszcze nikt o niego nie pytał.
///
/// Leniwie, bo teksty bywają potrzebne, zanim program zdąży cokolwiek zrobić —
/// wiersz poleceń wypisuje pomoc jeszcze przed wejściem do `main`. Dwa wątki
/// mogą tu wejść naraz i nic się nie stanie: policzą to samo.
fn ensure_loaded() {
    if CURRENT.load(Ordering::Relaxed) != UNSET {
        return;
    }
    // Zmienna środowiskowa przykrywa także zapisany wybór — i pokazuje się na
    // liście, żeby okno nie twierdziło czegoś innego, niż widać na ekranie.
    if let Some(lang) = std::env::var("MICBRIDGE_LANG")
        .ok()
        .and_then(|v| Lang::from_tag(&v))
    {
        PICKED.store(lang.index(), Ordering::Relaxed);
        CURRENT.store(lang.index(), Ordering::Relaxed);
        return;
    }
    match saved() {
        Some(lang) => {
            PICKED.store(lang.index(), Ordering::Relaxed);
            CURRENT.store(lang.index(), Ordering::Relaxed);
        }
        None => {
            PICKED.store(AUTO, Ordering::Relaxed);
            CURRENT.store(Lang::automatic().index(), Ordering::Relaxed);
        }
    }
}

/// Język tej sesji.
pub fn lang() -> Lang {
    ensure_loaded();
    Lang::from_index(CURRENT.load(Ordering::Relaxed)).unwrap_or(Lang::En)
}

/// Co stoi na liście wyboru. `None` znaczy „automatycznie”.
pub fn picked() -> Option<Lang> {
    ensure_loaded();
    Lang::from_index(PICKED.load(Ordering::Relaxed))
}

/// Zmienia język i zapamiętuje wybór. `None` oddaje decyzję systemowi.
///
/// Działa od razu: okno czyta [`lang`] przy każdym rysowaniu, więc następna
/// klatka jest już w nowym języku. Wyjątkiem jest menu ikony w zasobniku —
/// jego napisy powstają raz, przy tworzeniu ikony, i zmieniają się dopiero po
/// ponownym uruchomieniu. Dwa słowa nie są warte rozbierania ikony na częściach
/// dwóch systemów naraz.
pub fn choose(choice: Option<Lang>) -> std::io::Result<()> {
    ensure_loaded();
    CURRENT.store(
        choice.unwrap_or_else(Lang::automatic).index(),
        Ordering::Relaxed,
    );
    PICKED.store(choice.map(Lang::index).unwrap_or(AUTO), Ordering::Relaxed);
    save(choice)
}

/// Plik z wyborem — obok kluczy parowania.
///
/// `MICBRIDGE_CONFIG` wskazuje plik z kluczami, więc bierzemy jego katalog:
/// testy i nietypowe ustawienia mają trzymać się kupy.
fn preference_path() -> Option<PathBuf> {
    let custom = std::env::var("MICBRIDGE_CONFIG").ok();
    beside_keys(custom.as_deref(), dirs::config_dir)
}

/// Gdzie leży plik z wyborem, przy zadanym otoczeniu.
///
/// Osobno od czytania i pisania, bo to jedyny kawałek, który zależy od
/// środowiska — i jedyny, w którym da się pomylić plik z katalogiem.
fn beside_keys(custom: Option<&str>, config_dir: impl Fn() -> Option<PathBuf>) -> Option<PathBuf> {
    match custom {
        Some(keys) => Path::new(keys).parent().map(|dir| dir.join("language")),
        None => Some(config_dir()?.join("micbridge").join("language")),
    }
}

fn saved() -> Option<Lang> {
    read_choice(&preference_path()?)
}

fn read_choice(path: &Path) -> Option<Lang> {
    Lang::from_tag(std::fs::read_to_string(path).ok()?.trim())
}

fn save(choice: Option<Lang>) -> std::io::Result<()> {
    let Some(path) = preference_path() else {
        return Ok(());
    };
    write_choice(&path, choice)
}

fn write_choice(path: &Path, choice: Option<Lang>) -> std::io::Result<()> {
    match choice {
        // Brak pliku znaczy „automatycznie”. Prościej niż zapisywać słowo,
        // które trzeba by potem odróżniać od nazwy języka.
        None => match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
        Some(lang) => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, lang.code())
        }
    }
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
    fn the_choice_lands_next_to_the_pairing_keys() {
        let none = || None;
        assert_eq!(
            beside_keys(Some("/home/kto/.config/micbridge/peers.toml"), none),
            Some(PathBuf::from("/home/kto/.config/micbridge/language")),
            "MICBRIDGE_CONFIG wskazuje plik, więc bierzemy jego katalog"
        );
        assert_eq!(
            beside_keys(None, || Some(PathBuf::from("/home/kto/.config"))),
            Some(PathBuf::from("/home/kto/.config/micbridge/language"))
        );
        assert_eq!(beside_keys(None, none), None, "bez katalogu nie ma gdzie");
    }

    #[test]
    fn a_written_choice_reads_back_and_erasing_it_means_automatic() {
        let dir = std::env::temp_dir().join("micbridge-test-jezyk");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("language");

        assert_eq!(
            read_choice(&path),
            None,
            "brak pliku znaczy „automatycznie”"
        );

        write_choice(&path, Some(Lang::De)).expect("zapis");
        assert_eq!(read_choice(&path), Some(Lang::De));

        write_choice(&path, Some(Lang::Uk)).expect("nadpisanie");
        assert_eq!(read_choice(&path), Some(Lang::Uk));

        write_choice(&path, None).expect("skasowanie");
        assert_eq!(read_choice(&path), None);
        // Kasowanie nieistniejącego pliku to nie błąd — inaczej wybranie
        // „automatycznie” dwa razy pod rząd zgłaszałoby awarię.
        write_choice(&path, None).expect("drugie skasowanie");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_language_survives_a_round_trip_through_its_code() {
        for lang in LANGS {
            assert_eq!(Lang::from_tag(lang.code()), Some(lang), "{lang:?}");
            assert_eq!(Lang::from_index(lang.index()), Some(lang), "{lang:?}");
        }
        assert_eq!(Lang::from_index(AUTO), None, "„automatycznie” to nie język");
        assert_eq!(Lang::from_index(UNSET), None);
    }

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
