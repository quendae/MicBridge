//! Zapamiętane klucze par.
//!
//! Parowanie odbywa się raz. Wynikiem jest wspólny sekret, który obie strony
//! zapisują u siebie pod nazwą drugiej maszyny — od kolejnego uruchomienia
//! łączą się bez pytania o cokolwiek.
//!
//! Nazwa jest tylko etykietą do wyszukania. Uwierzytelnia klucz: kto go nie ma,
//! ten nie przejdzie uzgodnienia, choćby podał się za dowolną maszynę.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

/// Długość wspólnego sekretu. Tyle bierze Noise jako PSK.
pub const KEY_LEN: usize = 32;

pub type Key = [u8; KEY_LEN];

#[derive(Default, Serialize, Deserialize)]
struct Store {
    /// nazwa maszyny → klucz w zapisie szesnastkowym
    peers: BTreeMap<String, String>,
}

/// Plik z kluczami w katalogu konfiguracyjnym użytkownika.
pub struct KeyStore {
    path: PathBuf,
    store: Store,
}

impl KeyStore {
    /// Wczytuje magazyn albo zaczyna pusty, gdy pliku jeszcze nie ma.
    pub fn open() -> Result<Self> {
        let path = default_path()?;
        let store = match std::fs::read(&path) {
            Ok(bytes) => toml::from_str(&String::from_utf8_lossy(&bytes))
                .with_context(|| format!("nie rozumiem {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Store::default(),
            Err(e) => return Err(anyhow!("nie mogę odczytać {}: {e}", path.display())),
        };
        Ok(Self { path, store })
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn get(&self, peer: &str) -> Option<Key> {
        let hex = self.store.peers.get(&normalize(peer))?;
        let bytes = hex::decode(hex).ok()?;
        bytes.try_into().ok()
    }

    pub fn peers(&self) -> impl Iterator<Item = &str> {
        self.store.peers.keys().map(String::as_str)
    }

    pub fn set(&mut self, peer: &str, key: &Key) -> Result<()> {
        self.store.peers.insert(normalize(peer), hex::encode(key));
        self.save()
    }

    pub fn forget(&mut self, peer: &str) -> Result<bool> {
        let removed = self.store.peers.remove(&normalize(peer)).is_some();
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    fn save(&self) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("nie mogę utworzyć {}", dir.display()))?;
        }
        let text = toml::to_string_pretty(&self.store).map_err(|e| anyhow!("zapis kluczy: {e}"))?;
        // Zapis przez plik tymczasowy: przerwany zapis nie może zostawić
        // magazynu w połowie i skasować działających parowań.
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, text)
            .with_context(|| format!("nie mogę zapisać {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("nie mogę zapisać {}", self.path.display()))?;
        restrict(&self.path);
        Ok(())
    }
}

/// Nazwy maszyn bywają pisane raz tak, raz siak — klucz ma się znaleźć i wtedy.
fn normalize(peer: &str) -> String {
    peer.trim().to_lowercase()
}

fn default_path() -> Result<PathBuf> {
    if let Ok(custom) = std::env::var("MICBRIDGE_CONFIG") {
        return Ok(PathBuf::from(custom));
    }
    let dir = dirs::config_dir()
        .ok_or_else(|| anyhow!("{}", mb_i18n::t(mb_i18n::Key::ErrNoConfigDir)))?;
    Ok(dir.join("micbridge").join("peers.toml"))
}

/// Odbiera prawa wszystkim poza właścicielem. W Windows dziedziczone ACL-e
/// katalogu użytkownika załatwiają to same, więc tam nie ma co robić.
#[cfg(unix)]
fn restrict(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict(_path: &std::path::Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> KeyStore {
        // Licznik, nie zegar. Zegar systemowy w Windows tyka co kilkanaście
        // milisekund, więc dwa testy startujące w tej samej chwili dostawały
        // tę samą nazwę pliku i deptały sobie po nim — a że testy chodzą
        // równolegle w jednym procesie, numer procesu ich nie rozróżniał.
        // Stąd brała się zagadka „raz przechodzi, raz nie”.
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "micbridge-test-{}-{}.toml",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        KeyStore {
            path,
            store: Store::default(),
        }
    }

    #[test]
    fn a_stored_key_survives_a_reopen() {
        let mut store = temp_store();
        let path = store.path().to_path_buf();
        let key = [7u8; KEY_LEN];
        store.set("Salon-PC", &key).unwrap();

        let reopened = KeyStore {
            store: toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap(),
            path: path.clone(),
        };
        assert_eq!(
            reopened.get("salon-pc"),
            Some(key),
            "wielkość liter nie liczy się"
        );
        assert_eq!(reopened.get("ktos-inny"), None);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn forgetting_reports_whether_there_was_anything_to_forget() {
        let mut store = temp_store();
        store.set("salon", &[1u8; KEY_LEN]).unwrap();
        assert!(store.forget("SALON").unwrap());
        assert!(!store.forget("salon").unwrap());
        std::fs::remove_file(store.path()).ok();
    }
}
