//! Parowanie: krótki kod z ekranu zamieniony na wspólny sekret.
//!
//! Sześć cyfr to za mało, żeby użyć ich wprost jako klucza — ale SPAKE2 nie
//! pozwala ich łamać offline. Podsłuchujący, który nagra całą wymianę, nie
//! dowie się z niej niczego, co pozwoliłoby zgadywać kod w domu; musi zgadnąć
//! za pierwszym razem, na żywo, a trzy pudła i odbiornik losuje nowy kod.
//!
//! Po uzgodnieniu obie strony wymieniają potwierdzenie. Bez niego zły kod
//! wyszedłby dopiero przy pierwszym pakiecie mediów, jako „coś nie działa”.

use std::io::{Read, Write};

use anyhow::{anyhow, bail, Result};
use rand::Rng;
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};

use mb_i18n::{t, t1, Key as K};
use mb_proto::ControlMsg;

use crate::keys::{Key, KEY_LEN};

/// Ile cyfr ma kod. Sześć to kompromis: da się przepisać z ekranu bez błędu,
/// a przy trzech próbach na żywo szansa trafienia to trzy na milion.
pub const CODE_DIGITS: usize = 6;

/// Ile razy wolno się pomylić, zanim kod przestaje być ważny.
pub const MAX_ATTEMPTS: u32 = 3;

/// Losuje kod do przepisania z ekranu.
pub fn fresh_code() -> String {
    let mut rng = rand::thread_rng();
    (0..CODE_DIGITS)
        .map(|_| char::from(b'0' + rng.gen_range(0..10)))
        .collect()
}

/// Rozbija kod na grupy po trzy cyfry — tak się go przepisuje bez gubienia.
pub fn format_code(code: &str) -> String {
    code.as_bytes()
        .chunks(3)
        .map(|c| String::from_utf8_lossy(c).to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Wyrzuca z wpisanego kodu wszystko, co nie jest cyfrą.
pub fn normalize_code(input: &str) -> String {
    input.chars().filter(char::is_ascii_digit).collect()
}

/// Strona nadająca: przepisuje kod z ekranu drugiej maszyny i zaczyna wymianę.
pub fn initiator<S: Read + Write>(stream: &mut S, code: &str) -> Result<Key> {
    let (state, mine) = Spake2::<Ed25519Group>::start_a(
        &Password::new(code.as_bytes()),
        &Identity::new(ID_SENDER),
        &Identity::new(ID_RECEIVER),
    );
    ControlMsg::Handshake { msg: mine }.write_to(stream)?;

    let theirs = expect_handshake(stream)?;
    let shared = state
        .finish(&theirs)
        .map_err(|e| anyhow!("wymiana kluczy się nie powiodła: {e:?}"))?;
    let key = derive(&shared);

    // Nadajnik potwierdza pierwszy: odbiornik ma prawo zamknąć połączenie
    // i policzyć nieudaną próbę, zanim cokolwiek o sobie powie.
    ControlMsg::Confirm {
        mac: confirm(&key, ID_SENDER),
    }
    .write_to(stream)?;

    let theirs = expect_confirm(stream)?;
    verify(&theirs, &confirm(&key, ID_RECEIVER))?;
    Ok(key)
}

/// Strona odbierająca: pokazuje kod i czeka.
pub fn responder<S: Read + Write>(stream: &mut S, code: &str) -> Result<Key> {
    let theirs = expect_handshake(stream)?;

    let (state, mine) = Spake2::<Ed25519Group>::start_b(
        &Password::new(code.as_bytes()),
        &Identity::new(ID_SENDER),
        &Identity::new(ID_RECEIVER),
    );
    ControlMsg::Handshake { msg: mine }.write_to(stream)?;

    let shared = state
        .finish(&theirs)
        .map_err(|e| anyhow!("wymiana kluczy się nie powiodła: {e:?}"))?;
    let key = derive(&shared);

    let theirs = expect_confirm(stream)?;
    verify(&theirs, &confirm(&key, ID_SENDER))?;
    ControlMsg::Confirm {
        mac: confirm(&key, ID_RECEIVER),
    }
    .write_to(stream)?;
    Ok(key)
}

const ID_SENDER: &[u8] = b"micbridge-nadajnik";
const ID_RECEIVER: &[u8] = b"micbridge-odbiornik";

/// Skraca wynik SPAKE2 do klucza o ustalonej długości i odcina go etykietą od
/// wszystkiego innego, co kiedykolwiek policzymy z tego samego sekretu.
fn derive(shared: &[u8]) -> Key {
    let mut h = Sha256::new();
    h.update(b"micbridge/psk/v1");
    h.update(shared);
    let out = h.finalize();
    let mut key = [0u8; KEY_LEN];
    key.copy_from_slice(&out[..KEY_LEN]);
    key
}

fn confirm(key: &Key, who: &[u8]) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(b"micbridge/potwierdzenie/v1");
    h.update(who);
    h.update(key);
    h.finalize().to_vec()
}

/// Porównanie w stałym czasie: różnica ma nie wyciekać przez to, jak szybko
/// odpowiadamy.
fn verify(got: &[u8], want: &[u8]) -> Result<()> {
    let same =
        got.len() == want.len() && got.iter().zip(want).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0;
    if same {
        Ok(())
    } else {
        bail!("{}", t(K::ErrCodeMismatch))
    }
}

fn expect_handshake<S: Read>(stream: &mut S) -> Result<Vec<u8>> {
    match ControlMsg::read_from(stream)? {
        ControlMsg::Handshake { msg } => Ok(msg),
        ControlMsg::Reject { reason } => bail!("{}", t1(K::ErrPeerAbortedPairing, reason)),
        other => bail!("oczekiwałem kroku parowania, dostałem {other:?}"),
    }
}

fn expect_confirm<S: Read>(stream: &mut S) -> Result<Vec<u8>> {
    match ControlMsg::read_from(stream)? {
        ControlMsg::Confirm { mac } => Ok(mac),
        ControlMsg::Reject { reason } => bail!("{}", t1(K::ErrPeerAbortedPairing, reason)),
        other => bail!("oczekiwałem potwierdzenia, dostałem {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    /// Para połączonych gniazd — parowanie chodzi po prawdziwym strumieniu,
    /// więc i test niech chodzi.
    fn socket_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::thread::spawn(move || TcpStream::connect(addr).unwrap());
        let (server, _) = listener.accept().unwrap();
        (client.join().unwrap(), server)
    }

    fn run(code_a: &str, code_b: &str) -> (Result<Key>, Result<Key>) {
        let (mut a, mut b) = socket_pair();
        let code_a = code_a.to_string();
        let sender = std::thread::spawn(move || initiator(&mut a, &code_a));
        let receiver = responder(&mut b, code_b);
        // Odbiornik, który odrzucił kod, zamyka połączenie — bez tego nadajnik
        // czekałby na odpowiedź, która nigdy nie przyjdzie.
        drop(b);
        (sender.join().unwrap(), receiver)
    }

    #[test]
    fn the_same_code_gives_both_sides_the_same_key() {
        let (a, b) = run("482193", "482193");
        assert_eq!(a.unwrap(), b.unwrap());
    }

    #[test]
    fn a_wrong_code_is_caught_by_the_confirmation() {
        let (a, b) = run("482193", "111111");
        // Odbiornik sprawdza pierwszy, więc to on melduje niezgodność;
        // nadajnik dowiaduje się, bo połączenie znika mu spod rąk.
        assert!(b.is_err(), "odbiornik ma odrzucić zły kod");
        assert!(a.is_err(), "nadajnik nie może uznać sesji za udaną");
    }

    #[test]
    fn codes_are_six_digits_and_read_like_a_phone_number() {
        let code = fresh_code();
        assert_eq!(code.len(), CODE_DIGITS);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
        assert_eq!(format_code("482193"), "482 193");
        assert_eq!(normalize_code(" 482-193 "), "482193");
    }
}
