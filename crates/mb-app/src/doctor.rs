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

use mb_i18n::{t, t1, t2, Key as K};

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
                t(K::DocMic),
                t2(K::DocDefaultIs, mb_i18n::devices(devices.len()), default),
            )
        }
        Ok(_) => bad(
            // Brak mikrofonu nie przeszkadza w odbieraniu, a to bywa cała rola
            // tej maszyny — stąd ostrzeżenie, nie błąd.
            Grade::Warn,
            t(K::DocMic),
            t(K::DocNoInput),
            t(K::DocNoInputAdvice),
        ),
        Err(e) => bad(
            Grade::Fail,
            t(K::DocMic),
            t1(K::DocCannotQuery, e),
            t(K::DocCheckAudio),
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
        Ok(out) if out.status.success() => ok(t(K::DocVirtualInput), t(K::DocPipewireOk)),
        _ => bad(
            Grade::Warn,
            t(K::DocVirtualInput),
            t(K::DocNoPipewire),
            t(K::DocNoPipewireAdvice),
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
                Some(cable) => ok(t(K::DocVirtualInput), t1(K::DocCableReady, &cable.name)),
                None => bad(
                    // Bez kabla ta maszyna wciąż może wysyłać własny mikrofon,
                    // i dla wielu osób to jedyne, czego chcą.
                    Grade::Warn,
                    t(K::DocVirtualInput),
                    t(K::DocNoCable),
                    t(K::DocNoCableAdvice),
                ),
            }
        }
        Err(e) => bad(
            Grade::Fail,
            t(K::DocVirtualInput),
            t1(K::DocCannotQuery, e),
            t(K::DocCheckAudio),
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
                    t(K::DocPairing),
                    t(K::DocNothingPaired),
                    t(K::DocNothingPairedAdvice),
                )
            } else {
                ok(t(K::DocPairing), peers.join(", "))
            }
        }
        Err(e) => bad(
            Grade::Fail,
            t(K::DocPairing),
            t1(K::DocCannotReadKeys, e),
            t(K::DocCheckConfigDir),
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
    fn advice_shows_up_only_when_there_is_something_to_do() {
        assert!(!ok("a", "jest").to_string().contains('\n'));
        let with_advice = bad(Grade::Warn, "b", "brak", "zainstaluj").to_string();
        assert!(with_advice.contains("zainstaluj"));
    }
}
