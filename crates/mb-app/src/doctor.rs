//! Sprawdzenie, czy maszyna jest gotowa — osobno do wysyłania i do odbierania.
//!
//! Wymagania obu ról są różne i to jest źródłem większości nieporozumień:
//! wysyłanie potrzebuje tylko mikrofonu, odbieranie potrzebuje miejsca, w które
//! da się wpuścić cudzy dźwięk tak, żeby wyglądał na mikrofon. W Linuksie to
//! drugie robimy sami, w Windows wymaga cudzego sterownika.
//!
//! Wykrywanie opiera się na liście urządzeń, a nie na rejestrze czy plikach
//! sterownika: pytamy dokładnie o to, czego program będzie potem używał, więc
//! odpowiedź nie może się rozminąć z rzeczywistością.

use std::fmt;

/// Jak wypadła jedna rzecz do sprawdzenia.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grade {
    /// Jest i działa.
    Ok,
    /// Da się bez tego żyć, ale warto wiedzieć.
    Warn,
    /// Ta rola nie zadziała, dopóki się tego nie naprawi.
    Fail,
}

impl Grade {
    pub fn mark(self) -> &'static str {
        match self {
            Grade::Ok => "✓",
            Grade::Warn => "•",
            Grade::Fail => "✗",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Check {
    pub name: String,
    pub grade: Grade,
    /// Co dokładnie zastaliśmy albo czego brakuje.
    pub detail: String,
    /// Co z tym zrobić. Puste, gdy nie ma nic do roboty.
    pub advice: String,
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {:<24} {}", self.grade.mark(), self.name, self.detail)?;
        if !self.advice.is_empty() {
            write!(f, "\n    {}", self.advice)?;
        }
        Ok(())
    }
}

/// Polska odmiana liczebnika. „1 urządzeń” w programie, który poza tym mówi
/// po ludzku, wygląda jak niedokończona robota.
fn devices_count(n: usize) -> String {
    let form = match (n % 10, n % 100) {
        (1, 11) => "urządzeń",
        (1, _) => "urządzenie",
        (2..=4, 12..=14) => "urządzeń",
        (2..=4, _) => "urządzenia",
        _ => "urządzeń",
    };
    format!("{n} {form}")
}

fn ok(name: &str, detail: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        grade: Grade::Ok,
        detail: detail.into(),
        advice: String::new(),
    }
}

fn bad(grade: Grade, name: &str, detail: impl Into<String>, advice: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        grade,
        detail: detail.into(),
        advice: advice.into(),
    }
}

/// Pełny przegląd: co działa, czego brakuje i dla której roli.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub checks: Vec<Check>,
}

impl Report {
    /// Najgorsza ocena w zestawie — po niej poznajemy, czy jest o czym mówić.
    pub fn worst(&self) -> Grade {
        if self.checks.iter().any(|c| c.grade == Grade::Fail) {
            Grade::Fail
        } else if self.checks.iter().any(|c| c.grade == Grade::Warn) {
            Grade::Warn
        } else {
            Grade::Ok
        }
    }
}

/// Sprawdza wszystko, co da się sprawdzić bez nawiązywania połączenia.
pub fn check() -> Report {
    let mut checks = vec![microphone(), sink()];
    checks.push(paired());
    Report { checks }
}

/// Czy jest z czego nadawać.
fn microphone() -> Check {
    match mb_audio::list(mb_audio::Direction::Input) {
        Ok(devices) if !devices.is_empty() => {
            let default = devices
                .iter()
                .find(|d| d.is_default)
                .map(|d| d.name.clone())
                .unwrap_or_else(|| devices[0].name.clone());
            ok(
                "mikrofon",
                format!("{}, domyślne: {default}", devices_count(devices.len())),
            )
        }
        Ok(_) => bad(
            // Brak mikrofonu nie przeszkadza w odbieraniu, a to bywa cała rola
            // tej maszyny — stąd ostrzeżenie, nie błąd.
            Grade::Warn,
            "mikrofon",
            "system nie pokazuje żadnego wejścia",
            "Ta maszyna może tylko odbierać. Do sprawdzenia ścieżki bez \
             mikrofonu jest `--device tone`.",
        ),
        Err(e) => bad(
            Grade::Fail,
            "mikrofon",
            format!("nie mogę odpytać systemu: {e}"),
            "Sprawdź, czy działa podsystem dźwięku.",
        ),
    }
}

/// Czy jest gdzie wpuścić cudzy dźwięk, żeby wyglądał na mikrofon.
#[cfg(target_os = "linux")]
fn sink() -> Check {
    // W Linuksie tworzymy własny węzeł, więc pytanie brzmi wyłącznie, czy
    // PipeWire w ogóle działa. Nie ma czego instalować.
    match std::process::Command::new("pw-cli")
        .arg("--version")
        .output()
    {
        Ok(out) if out.status.success() => ok(
            "wejście wirtualne",
            "PipeWire działa — mikrofon „MicBridge” powstaje sam",
        ),
        _ => bad(
            Grade::Warn,
            "wejście wirtualne",
            "nie widzę PipeWire",
            "Odbieranie wymaga PipeWire: `systemctl --user status pipewire \
             wireplumber`. Na czystym PulseAudio wskaż ujście przez --sink.",
        ),
    }
}

#[cfg(not(target_os = "linux"))]
fn sink() -> Check {
    // Windows nie pozwala programowi utworzyć mikrofonu bez sterownika trybu
    // jądra z podpisem, więc pytamy o cudzy kabel. Pytamy listę urządzeń, bo
    // to dokładnie ta sama lista, z której program potem skorzysta — rejestr
    // czy pliki sterownika mogłyby się z nią rozminąć.
    match mb_audio::list(mb_audio::Direction::Output) {
        Ok(devices) => {
            match devices
                .iter()
                .find(|d| mb_audio::looks_like_virtual_cable(&d.name))
            {
                Some(cable) => ok("wejście wirtualne", format!("{} — gotowe", cable.name)),
                None => bad(
                    // Bez kabla ta maszyna wciąż może wysyłać własny mikrofon,
                    // i dla wielu osób to jedyne, czego chcą.
                    Grade::Warn,
                    "wejście wirtualne",
                    "brak wirtualnego kabla",
                    "Potrzebny tylko do ODBIERANIA — żeby cudzy mikrofon \
                     pojawił się tu jako urządzenie.\n    \
                     Zainstaluj VB-CABLE (https://vb-audio.com/Cable/) i ustaw \
                     w nim Max Latency na 2048.\n    \
                     Do samego wysyłania nie jest potrzebny.",
                ),
            }
        }
        Err(e) => bad(
            Grade::Fail,
            "wejście wirtualne",
            format!("nie mogę odpytać systemu: {e}"),
            "Sprawdź, czy działa podsystem dźwięku.",
        ),
    }
}

/// Z kim ta maszyna jest już sparowana.
fn paired() -> Check {
    match mb_net::KeyStore::open() {
        Ok(store) => {
            let peers: Vec<&str> = store.peers().collect();
            if peers.is_empty() {
                bad(
                    Grade::Warn,
                    "parowanie",
                    "nic jeszcze nie sparowane",
                    "Dzieje się samo przy pierwszym połączeniu: odbiornik pokaże \
                     kod, nadajnik o niego poprosi.",
                )
            } else {
                ok("parowanie", peers.join(", "))
            }
        }
        Err(e) => bad(
            Grade::Fail,
            "parowanie",
            format!("nie mogę odczytać kluczy: {e}"),
            "Sprawdź prawa do katalogu konfiguracyjnego.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_worst_grade_wins() {
        let mut report = Report::default();
        assert_eq!(
            report.worst(),
            Grade::Ok,
            "pusty przegląd nie ma zastrzeżeń"
        );

        report.checks.push(ok("a", "jest"));
        assert_eq!(report.worst(), Grade::Ok);

        report
            .checks
            .push(bad(Grade::Warn, "b", "brak", "zainstaluj"));
        assert_eq!(report.worst(), Grade::Warn);

        report.checks.push(bad(Grade::Fail, "c", "padło", "napraw"));
        assert_eq!(report.worst(), Grade::Fail, "błąd przykrywa ostrzeżenie");
    }

    #[test]
    fn numbers_take_the_right_form() {
        assert_eq!(devices_count(1), "1 urządzenie");
        assert_eq!(devices_count(2), "2 urządzenia");
        assert_eq!(devices_count(5), "5 urządzeń");
        // Nastolatki są wyjątkiem: dwanaście urządzeń, nie dwanaście urządzenia.
        assert_eq!(devices_count(12), "12 urządzeń");
        assert_eq!(devices_count(22), "22 urządzenia");
        assert_eq!(devices_count(0), "0 urządzeń");
    }

    #[test]
    fn advice_shows_up_only_when_there_is_something_to_do() {
        assert!(!ok("a", "jest").to_string().contains('\n'));
        let with_advice = bad(Grade::Warn, "b", "brak", "zainstaluj").to_string();
        assert!(with_advice.contains("zainstaluj"));
    }
}
