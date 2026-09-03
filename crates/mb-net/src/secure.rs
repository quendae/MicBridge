//! Szyfrowanie sesji: Noise na kanale sterującym, AEAD na mediach.
//!
//! Parowanie daje wspólny sekret, który żyje długo. Na jego podstawie każde
//! połączenie uzgadnia świeże klucze przez Noise `NNpsk0` — dzięki temu nagrany
//! ruch nie da się odszyfrować nawet komuś, kto później zdobędzie ten sekret.
//!
//! Media idą po UDP, gdzie pakiety gubią się i przestawiają, więc nie mogą
//! nieść stanu strumienia szyfrującego. Każdy pakiet szyfrujemy osobno,
//! a wartość jednorazową bierzemy z numeru sekwencyjnego. Nagłówek RTP zostaje
//! jawny — bufor jitter musi go czytać, zanim cokolwiek odszyfrujemy — ale
//! wchodzi do uwierzytelnienia, więc podmiana numeru unieważnia pakiet.

use std::io::{Read, Write};

use anyhow::{anyhow, bail, Result};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};

use mb_proto::ControlMsg;

use crate::keys::{Key, KEY_LEN};

/// Bez podpisów i tożsamości długoterminowych: uwierzytelnia sam wspólny
/// sekret z parowania, a klucze sesji są jednorazowe.
const NOISE_PARAMS: &str = "Noise_NNpsk0_25519_ChaChaPoly_BLAKE2s";

/// Noise nie przepuści dłuższej wiadomości, a nasze ramki sterujące mają po
/// kilkadziesiąt bajtów — zapas jest tu wyłącznie po to, by błąd był jasny.
const MAX_NOISE_MSG: usize = 65535;
const MAX_PLAIN: usize = MAX_NOISE_MSG - 16;

/// Ile bajtów dokłada uwierzytelnienie pakietu mediów.
pub const MEDIA_TAG_LEN: usize = 16;

/// Zaszyfrowany kanał sterujący.
///
/// Trzyma tylko szyfrowanie; ramkowanie i gniazdo zostają na zewnątrz, bo
/// nadajnik czyta z osobnego wątku i nie może trzymać zamka na czas czekania
/// na dane.
pub struct SecureChannel {
    noise: snow::TransportState,
}

impl SecureChannel {
    /// Strona nadająca zaczyna uzgodnienie.
    pub fn initiator<S: Read + Write>(stream: &mut S, psk: &Key) -> Result<Self> {
        let mut noise = builder(psk)?
            .build_initiator()
            .map_err(|e| anyhow!("nie mogę zacząć uzgodnienia: {e}"))?;

        let mut buf = vec![0u8; MAX_NOISE_MSG];
        let n = noise
            .write_message(&[], &mut buf)
            .map_err(|e| anyhow!("uzgodnienie: {e}"))?;
        ControlMsg::Handshake {
            msg: buf[..n].to_vec(),
        }
        .write_to(stream)?;

        let theirs = expect_handshake(stream)?;
        noise.read_message(&theirs, &mut buf).map_err(wrong_key)?;

        Ok(Self {
            noise: noise
                .into_transport_mode()
                .map_err(|e| anyhow!("uzgodnienie nie doszło do końca: {e}"))?,
        })
    }

    /// Strona odbierająca odpowiada.
    pub fn responder<S: Read + Write>(stream: &mut S, psk: &Key) -> Result<Self> {
        let mut noise = builder(psk)?
            .build_responder()
            .map_err(|e| anyhow!("nie mogę zacząć uzgodnienia: {e}"))?;

        let mut buf = vec![0u8; MAX_NOISE_MSG];
        let theirs = expect_handshake(stream)?;
        noise.read_message(&theirs, &mut buf).map_err(wrong_key)?;

        let n = noise
            .write_message(&[], &mut buf)
            .map_err(|e| anyhow!("uzgodnienie: {e}"))?;
        ControlMsg::Handshake {
            msg: buf[..n].to_vec(),
        }
        .write_to(stream)?;

        Ok(Self {
            noise: noise
                .into_transport_mode()
                .map_err(|e| anyhow!("uzgodnienie nie doszło do końca: {e}"))?,
        })
    }

    /// Zamienia wiadomość w ciało ramki gotowe do wysłania.
    pub fn seal(&mut self, msg: &ControlMsg) -> Result<Vec<u8>> {
        let mut plain = Vec::new();
        ciborium::into_writer(msg, &mut plain).map_err(|e| anyhow!("kodowanie: {e}"))?;
        if plain.len() > MAX_PLAIN {
            bail!("wiadomość sterująca ma {} bajtów, za dużo", plain.len());
        }
        let mut out = vec![0u8; plain.len() + MEDIA_TAG_LEN];
        let n = self
            .noise
            .write_message(&plain, &mut out)
            .map_err(|e| anyhow!("szyfrowanie: {e}"))?;
        out.truncate(n);
        Ok(out)
    }

    /// Odwrotność `seal`.
    pub fn open(&mut self, frame: &[u8]) -> Result<ControlMsg> {
        let mut plain = vec![0u8; MAX_NOISE_MSG];
        let n = self
            .noise
            .read_message(frame, &mut plain)
            .map_err(|e| anyhow!("nie mogę odszyfrować wiadomości: {e}"))?;
        ciborium::from_reader(&plain[..n]).map_err(|e| anyhow!("dekodowanie: {e}"))
    }
}

fn builder(psk: &Key) -> Result<snow::Builder<'_>> {
    snow::Builder::new(
        NOISE_PARAMS
            .parse()
            .map_err(|e| anyhow!("zły opis Noise: {e}"))?,
    )
    .psk(0, psk)
    .map_err(|e| anyhow!("zły klucz sesji: {e}"))
}

/// Uzgodnienie z niepasującym sekretem wygląda jak uszkodzone dane — bez tego
/// tłumaczenia użytkownik dostawałby „decrypt error” zamiast wskazówki.
fn wrong_key(e: snow::Error) -> anyhow::Error {
    anyhow!(
        "uzgodnienie odrzucone ({e}). Najpewniej druga strona ma inny klucz — \
         sparujcie się od nowa: `micbridge forget <nazwa>` po obu stronach."
    )
}

fn expect_handshake<S: Read>(stream: &mut S) -> Result<Vec<u8>> {
    match ControlMsg::read_from(stream)? {
        ControlMsg::Handshake { msg } => Ok(msg),
        ControlMsg::Reject { reason } => bail!("druga strona przerwała: {reason}"),
        other => bail!("oczekiwałem uzgodnienia, dostałem {other:?}"),
    }
}

/// Szyfrowanie pojedynczych pakietów mediów.
///
/// Klucz losuje odbiornik na każdą sesję i podaje go zaszyfrowanym kanałem
/// sterującym, więc nie ma tu żadnego stanu do uzgadniania.
pub struct MediaCipher {
    aead: ChaCha20Poly1305,
}

impl MediaCipher {
    pub fn new(key: &[u8]) -> Result<Self> {
        if key.len() != KEY_LEN {
            bail!("klucz mediów ma {} bajtów zamiast {KEY_LEN}", key.len());
        }
        Ok(Self {
            aead: ChaCha20Poly1305::new(key.into()),
        })
    }

    /// Zwraca zaszyfrowany ładunek z doklejonym znacznikiem.
    pub fn seal(&self, seq: u64, header: &[u8], payload: &[u8]) -> Result<Vec<u8>> {
        self.aead
            .encrypt(
                &nonce(seq),
                Payload {
                    msg: payload,
                    aad: header,
                },
            )
            .map_err(|_| anyhow!("nie mogę zaszyfrować pakietu"))
    }

    /// Zwraca odszyfrowany ładunek albo błąd, jeśli pakiet nie jest nasz.
    pub fn open(&self, seq: u64, header: &[u8], sealed: &[u8]) -> Result<Vec<u8>> {
        self.aead
            .decrypt(
                &nonce(seq),
                Payload {
                    msg: sealed,
                    aad: header,
                },
            )
            .map_err(|_| anyhow!("pakiet nie przeszedł uwierzytelnienia"))
    }
}

/// Wartość jednorazowa z numeru pakietu.
///
/// Numer rozszerzony do 64 bitów nie powtórzy się w sesji — przy stu pakietach
/// na sekundę licznik starczy na miliardy lat — a klucz i tak jest nowy przy
/// każdym połączeniu.
fn nonce(seq: u64) -> Nonce {
    let mut bytes = [0u8; 12];
    bytes[4..].copy_from_slice(&seq.to_be_bytes());
    *Nonce::from_slice(&bytes)
}

/// Losuje klucz na jedną sesję mediów.
pub fn fresh_media_key() -> Key {
    let mut key = [0u8; KEY_LEN];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut key);
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    fn socket_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::thread::spawn(move || TcpStream::connect(addr).unwrap());
        let (server, _) = listener.accept().unwrap();
        (client.join().unwrap(), server)
    }

    fn handshake(psk_a: Key, psk_b: Key) -> (Result<SecureChannel>, Result<SecureChannel>) {
        let (mut a, mut b) = socket_pair();
        let sender = std::thread::spawn(move || {
            let ch = SecureChannel::initiator(&mut a, &psk_a);
            // Gniazdo musi przeżyć uzgodnienie po drugiej stronie.
            (ch, a)
        });
        let receiver = SecureChannel::responder(&mut b, &psk_b);
        drop(b);
        let (sender, _keep) = sender.join().unwrap();
        (sender, receiver)
    }

    #[test]
    fn a_shared_secret_gives_a_working_channel() {
        let psk = [9u8; KEY_LEN];
        let (a, b) = handshake(psk, psk);
        let (mut a, mut b) = (a.unwrap(), b.unwrap());

        let frame = a.seal(&ControlMsg::Mute { on: true }).unwrap();
        assert!(
            !frame.windows(4).any(|w| w == b"Mute"),
            "nazwa wariantu nie może być widoczna w ramce"
        );
        assert!(matches!(
            b.open(&frame).unwrap(),
            ControlMsg::Mute { on: true }
        ));
    }

    #[test]
    fn a_different_secret_does_not_get_through() {
        let (a, b) = handshake([1u8; KEY_LEN], [2u8; KEY_LEN]);
        assert!(a.is_err() || b.is_err(), "uzgodnienie ma paść");
    }

    #[test]
    fn a_media_packet_survives_the_round_trip() {
        let key = fresh_media_key();
        let cipher = MediaCipher::new(&key).unwrap();
        let header = [0x80, 0x6f, 0, 1, 0, 0, 0, 0, 0xAB, 0xCD, 0xEF, 0x01];
        let payload = b"ramka opusa";

        let sealed = cipher.seal(7, &header, payload).unwrap();
        assert_eq!(sealed.len(), payload.len() + MEDIA_TAG_LEN);
        assert_eq!(cipher.open(7, &header, &sealed).unwrap(), payload);
    }

    #[test]
    fn a_tampered_header_invalidates_the_packet() {
        let cipher = MediaCipher::new(&fresh_media_key()).unwrap();
        let header = [0x80, 0x6f, 0, 1, 0, 0, 0, 0, 0xAB, 0xCD, 0xEF, 0x01];
        let sealed = cipher.seal(7, &header, b"ramka").unwrap();

        let mut tampered = header;
        tampered[3] = 2; // podmieniony numer sekwencyjny
        assert!(cipher.open(7, &tampered, &sealed).is_err());
        // Ten sam pakiet podstawiony pod inny numer też ma odpaść.
        assert!(cipher.open(8, &header, &sealed).is_err());
    }
}
