//! Uzgodnienie sesji: kto z kim, jakim kluczem i czy trzeba się poznać.
//!
//! Parowanie odbywa się raz. Odbiornik pokazuje sześciocyfrowy kod, użytkownik
//! przepisuje go na drugiej maszynie i obie strony zapisują u siebie wspólny
//! sekret. Od następnego uruchomienia łączą się bez pytania o cokolwiek.
//!
//! Decyzję o parowaniu podejmuje nadajnik, bo tylko on ma obie informacje:
//! czy zna odbiornik i — z odpowiedzi odbiornika — czy odbiornik zna jego.
//! Wystarczy, że jedna strona straci klucz, i parujemy się od nowa.

use std::io::{BufRead, Read, Write};
use std::sync::Mutex;

use anyhow::{bail, Context, Result};

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

    fn note_failure(&mut self) {
        self.failures += 1;
        if self.failures >= mb_net::pair::MAX_ATTEMPTS {
            self.code = mb_net::pair::fresh_code();
            self.failures = 0;
            println!("  trzy nieudane próby — losuję nowy kod.");
        }
    }

    fn note_success(&mut self) {
        self.failures = 0;
    }
}

/// Strona nadająca: przedstawia się, w razie potrzeby paruje i zestawia
/// szyfrowany kanał.
pub fn establish<S: Read + Write>(stream: &mut S, code: Option<&str>) -> Result<SecureChannel> {
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
            let code = match code {
                Some(c) => mb_net::pair::normalize_code(c),
                None => ask_for_code(&peer)?,
            };
            let key = mb_net::pair::initiator(stream, &code)?;
            store.set(&peer, &key)?;
            println!("Sparowano z „{peer}”. Następnym razem pójdzie bez kodu.");
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
        println!("\n„{}” prosi o sparowanie.", init.host);
        println!("  KOD: {}", mb_net::pair::format_code(&code));
        println!("  Przepisz go na drugiej maszynie.");

        match mb_net::pair::responder(stream, &code) {
            Ok(key) => {
                if let Ok(mut p) = pairing.lock() {
                    p.note_success();
                }
                store.set(&init.host, &key)?;
                println!("  Sparowano z „{}”.", init.host);
                key
            }
            Err(e) => {
                if let Ok(mut p) = pairing.lock() {
                    p.note_failure();
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

fn ask_for_code(peer: &str) -> Result<String> {
    println!("\n„{peer}” nie jest jeszcze sparowany.");
    println!("Na jego ekranie pojawił się sześciocyfrowy kod.");
    print!("Przepisz go tutaj: ");
    std::io::stdout().flush().ok();

    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .context("nie mogę odczytać kodu")?;

    let code = mb_net::pair::normalize_code(&line);
    if code.len() != mb_net::pair::CODE_DIGITS {
        bail!(
            "kod ma {} cyfr, oczekuję {}",
            code.len(),
            mb_net::pair::CODE_DIGITS
        );
    }
    Ok(code)
}
