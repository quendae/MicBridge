//! Uruchamianie razem z systemem — opcja, nie domyślne zachowanie.
//!
//! Każdy system robi to inaczej i żaden nie potrzebuje do tego biblioteki:
//! w Windows to wpis w rejestrze, w Linuksie plik `.desktop` w katalogu
//! autostartu. Obie drogi da się cofnąć jednym skasowaniem. W macOS byłby to
//! LaunchAgent — jeszcze go nie ma, więc tam opcja uczciwie odmawia.

#[cfg(not(target_os = "macos"))]
use anyhow::Context;
use anyhow::Result;

/// Pod tą nazwą wpis widnieje w systemie.
#[cfg(not(target_os = "macos"))]
const ENTRY: &str = "MicBridge";

/// Czy jesteśmy ustawieni na start razem z systemem.
pub fn enabled() -> bool {
    imp::enabled().unwrap_or(false)
}

/// Włącza albo wyłącza autostart.
pub fn set(on: bool) -> Result<()> {
    if on {
        imp::enable()
    } else {
        imp::disable()
    }
}

#[cfg(not(target_os = "macos"))]
fn exe() -> Result<std::path::PathBuf> {
    std::env::current_exe().context(mb_i18n::t(mb_i18n::Key::ErrOwnPath))
}

#[cfg(windows)]
mod imp {
    use super::{exe, Result, ENTRY};
    use std::process::Command;

    /// Klucz „Run” bieżącego użytkownika: nie wymaga uprawnień administratora
    /// i znika razem z jego profilem.
    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

    pub fn enabled() -> Result<bool> {
        let out = Command::new("reg")
            .args(["query", KEY, "/v", ENTRY])
            .output()?;
        Ok(out.status.success())
    }

    pub fn enable() -> Result<()> {
        let path = exe()?;
        // Cudzysłowy, bo ścieżka niemal zawsze zawiera spację.
        let value = format!("\"{}\"", path.display());
        let out = Command::new("reg")
            .args(["add", KEY, "/v", ENTRY, "/t", "REG_SZ", "/d", &value, "/f"])
            .output()?;
        if !out.status.success() {
            anyhow::bail!("{}", mb_i18n::t(mb_i18n::Key::ErrAutostartWrite));
        }
        Ok(())
    }

    pub fn disable() -> Result<()> {
        let out = Command::new("reg")
            .args(["delete", KEY, "/v", ENTRY, "/f"])
            .output()?;
        // Brak wpisu to nie błąd — chcieliśmy, żeby go nie było.
        let _ = out;
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::Result;

    pub fn enabled() -> Result<bool> {
        Ok(false)
    }

    pub fn enable() -> Result<()> {
        anyhow::bail!("{}", mb_i18n::t(mb_i18n::Key::MacNoAutostart))
    }

    /// Nie było czego włączać, więc nie ma czego wyłączać.
    pub fn disable() -> Result<()> {
        Ok(())
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod imp {
    use super::{exe, Result, ENTRY};
    use std::path::PathBuf;

    fn desktop_file() -> Result<PathBuf> {
        let dir = dirs_config()?.join("autostart");
        Ok(dir.join("micbridge.desktop"))
    }

    /// XDG mówi: `$XDG_CONFIG_HOME`, a gdy go nie ma — `~/.config`.
    fn dirs_config() -> Result<PathBuf> {
        if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
            if !dir.is_empty() {
                return Ok(PathBuf::from(dir));
            }
        }
        let home = std::env::var("HOME")
            .map_err(|_| anyhow::anyhow!("{}", mb_i18n::t(mb_i18n::Key::ErrNoHome)))?;
        Ok(PathBuf::from(home).join(".config"))
    }

    pub fn enabled() -> Result<bool> {
        Ok(desktop_file()?.exists())
    }

    pub fn enable() -> Result<()> {
        let path = desktop_file()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let exe = exe()?;
        let contents = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name={ENTRY}\n\
             Comment=Mikrofon z drugiego komputera\n\
             Exec={}\n\
             Terminal=false\n\
             X-GNOME-Autostart-enabled=true\n",
            exe.display()
        );
        std::fs::write(&path, contents)?;
        Ok(())
    }

    pub fn disable() -> Result<()> {
        match std::fs::remove_file(desktop_file()?) {
            Ok(()) => Ok(()),
            // Brak pliku to nie błąd — chcieliśmy, żeby go nie było.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn on_macos_autostart_is_off_and_refuses_to_turn_on() {
        assert!(
            !enabled(),
            "w macOS autostartu nie ma, więc nie może być włączony"
        );
        let err = set(true).expect_err("włączenie ma odmówić, nie udawać, że się udało");
        assert_eq!(err.to_string(), mb_i18n::t(mb_i18n::Key::MacNoAutostart));
        assert!(
            set(false).is_ok(),
            "wyłączenie czegoś, czego nie ma, nie jest błędem"
        );
    }
}
