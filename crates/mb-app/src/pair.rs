//! Uzgodnienie sesji: kto z kim, jakim kluczem i czy trzeba się poznać.
//!
//! Parowanie odbywa się raz. Odbiornik pokazuje sześciocyfrowy kod, użytkownik
//! przepisuje go na drugiej maszynie i obie strony zapisują u siebie wspólny
//! sekret. Od następnego uruchomienia łączą się bez pytania o cokolwiek.
//!
//! Decyzję o parowaniu podejmuje nadajnik, bo tylko on ma obie informacje:
//! czy zna odbiornik i — z odpowiedzi odbiornika — czy odbiornik zna jego.
//! Wystarczy, że jedna strona straci klucz, i parujemy się od nowa.

use std::io::{Read, Write};
use std::sync::Mutex;

use anyhow::{bail, Result};

use crate::ui::Reporter;
use mb_net::{KeyStore, SecureChannel};
use mb_proto::{read_frame, write_frame, ControlMsg, Init, PROTOCOL_VERSION};

/// Stan parowania po stronie odbiornika, wspólny dla kolejnych połączeń.
///
/// Kod żyje dłużej niż jedna próba, bo użytkownik może się pomylić przy
/// przepisywaniu. Ale nie dowolnie długo: po trzech pudłach losujemy nowy,
/// żeby zgadywanie na żywo nie miało jak się nazbierać.
pub struct Pairing {
    code: String,
    failures: u32,
}

impl Default for Pairing {
    fn default() -> Self {
        Self::new()
    }
}

impl Pairing {
    pub fn new() -> Self {
        Self {
            code: mb_net::pair::fresh_code(),
            failures: 0,
        }
    }

    fn code(&self) -> String {
        self.code.clone()
    }

    /// Zwraca true, gdy kod przepadł i jest nowy.
    fn note_failure(&mut self) -> bool {
        self.failures += 1;
        if self.failures < mb_net::pair::MAX_ATTEMPTS {
            return false;
        }
        self.code = mb_net::pair::fresh_code();
        self.failures = 0;
        true
    }

    fn note_success(&mut self) {
        self.failures = 0;
    }
}

/// Strona nadająca: przedstawia się, w razie potrzeby paruje i zestawia
/// szyfrowany kanał.
pub fn establish<S: Read + Write>(stream: &mut S, ui: &dyn Reporter) -> Result<SecureChannel> {
    ControlMsg::Init(Init {
        version: PROTOCOL_VERSION,
        host: mb_net::hostname(),
    })
    .write_to(stream)?;

    let (peer, knows_me) = match ControlMsg::read_from(stream)? {
        ControlMsg::Ready { host, known } => (host, known),
        ControlMsg::Reject { reason } => bail!("odbiornik odrzucił połączenie: {reason}"),
        other => bail!("nieoczekiwana odpowiedź na przedstawienie się: {other:?}"),
    };

    let mut store = KeyStore::open()?;
    let stored = store.get(&peer);
    let needed = !knows_me || stored.is_none();
    ControlMsg::Pairing { needed }.write_to(stream)?;

    let key = match stored {
        Some(key) if !needed => key,
        _ => {
            let code = ui.ask_code(&peer)?;
            let key = mb_net::pair::initiator(stream, &code)?;
            store.set(&peer, &key)?;
            ui.line(&format!(
                "Sparowano z „{peer}”. Następnym razem pójdzie bez kodu."
            ));
            key
        }
    };

    SecureChannel::initiator(stream, &key)
}

/// Strona odbierająca: rozpoznaje nadajnik, w razie potrzeby pokazuje kod.
///
/// Zwraca kanał i nazwę drugiej maszyny.
pub fn accept<S: Read + Write>(
    stream: &mut S,
    pairing: &Mutex<Pairing>,
    ui: &dyn Reporter,
) -> Result<(SecureChannel, String)> {
    let init = match ControlMsg::read_from(stream)? {
        ControlMsg::Init(i) => i,
        other => bail!("oczekiwałem przedstawienia się, dostałem {other:?}"),
    };
    if init.version != PROTOCOL_VERSION {
        let reason = format!(
            "wersja protokołu {} (obsługuję {PROTOCOL_VERSION})",
            init.version
        );
        let _ = ControlMsg::Reject {
            reason: reason.clone(),
        }
        .write_to(stream);
        bail!(reason);
    }

    let mut store = KeyStore::open()?;
    let stored = store.get(&init.host);
    ControlMsg::Ready {
        host: mb_net::hostname(),
        known: stored.is_some(),
    }
    .write_to(stream)?;

    let needed = match ControlMsg::read_from(stream)? {
        ControlMsg::Pairing { needed } => needed,
        other => bail!("oczekiwałem decyzji o parowaniu, dostałem {other:?}"),
    };

    let key = if needed {
        let code = {
            let Ok(p) = pairing.lock() else {
                bail!("stan parowania niedostępny")
            };
            p.code()
        };
        ui.show_code(&init.host, &code);

        match mb_net::pair::responder(stream, &code) {
            Ok(key) => {
                if let Ok(mut p) = pairing.lock() {
                    p.note_success();
                }
                store.set(&init.host, &key)?;
                ui.line(&format!("  Sparowano z „{}”.", init.host));
                key
            }
            Err(e) => {
                let fresh = match pairing.lock() {
                    Ok(mut p) => p.note_failure(),
                    Err(_) => false,
                };
                if fresh {
                    ui.line("  trzy nieudane próby — losuję nowy kod.");
                }
                let _ = ControlMsg::Reject {
                    reason: "parowanie odrzucone".into(),
                }
                .write_to(stream);
                return Err(e);
            }
        }
    } else {
        // Nadajnik twierdzi, że ma klucz. Jeśli my go nie mamy, nie ma z czego
        // zestawić sesji — mówimy to wprost, zamiast pozwolić uzgodnieniu paść
        // na niezrozumiałym błędzie kryptografii.
        match stored {
            Some(key) => key,
            None => {
                let reason = format!("nie znam „{}” — sparuj się od nowa", init.host);
                let _ = ControlMsg::Reject {
                    reason: reason.clone(),
                }
                .write_to(stream);
                bail!(reason);
            }
        }
    };

    let channel = SecureChannel::responder(stream, &key)?;
    Ok((channel, init.host))
}

/// Wysyła wiadomość zaszyfrowanym kanałem.
///
/// Zamek trzymamy wyłącznie na czas szyfrowania. Gdyby obejmował też zapis do
/// gniazda, wątek czytający statystyki blokowałby wysyłkę na czas swojego
/// czekania.
pub fn send_secure<W: Write>(
    stream: &mut W,
    channel: &Mutex<SecureChannel>,
    msg: &ControlMsg,
) -> Result<()> {
    let body = {
        let Ok(mut ch) = channel.lock() else {
            bail!("kanał sterujący niedostępny")
        };
        ch.seal(msg)?
    };
    write_frame(stream, &body)?;
    Ok(())
}

/// Odbiera wiadomość zaszyfrowanym kanałem.
pub fn recv_secure<R: Read>(stream: &mut R, channel: &Mutex<SecureChannel>) -> Result<ControlMsg> {
    let body = read_frame(stream)?;
    let Ok(mut ch) = channel.lock() else {
        bail!("kanał sterujący niedostępny")
    };
    ch.open(&body)
}
