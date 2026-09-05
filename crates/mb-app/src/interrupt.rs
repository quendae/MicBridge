//! Gniazdo sterujące, które da się przerwać.
//!
//! Sesja rozmawia z drugą maszyną po TCP i większość czasu spędza w czekaniu
//! na odpowiedź. Zwykłe `read` czeka bez końca — dopóki tamta strona się nie
//! odezwie albo nie zerwie połączenia. To wystarcza, gdy wszystko idzie
//! dobrze, ale nie wtedy, gdy użytkownik właśnie wyłącza program: flaga
//! zatrzymania leży nietknięta, bo nie ma kto na nią spojrzeć. Sesja czekająca
//! na przedstawienie się drugiej strony albo na przepisanie kodu parowania
//! potrafi tak stać tyle, ile ktoś siedzi przy klawiaturze — albo w ogóle,
//! gdy po drugiej stronie ktoś tylko otworzył połączenie i zamilkł.
//!
//! Rozwiązaniem jest limit czasu na odczyt i pętla wokół niego: gniazdo wraca
//! co [`POLL`], my patrzymy na flagę i albo czytamy dalej, albo się poddajemy.
//! Ważne, że limit zamyka się na zerze bajtów — gniazdo, które nic nie dało,
//! niczego też nie skonsumowało, więc powtórka nie gubi ani jednego bajtu
//! i ramka po drugiej stronie zostaje cała.
//!
//! Poddanie się melduje się jako [`io::ErrorKind::ConnectionAborted`], a nie
//! `Interrupted`. Ten drugi ma w bibliotece standardowej ustalone znaczenie
//! „spróbuj jeszcze raz" i `read_exact` ponawia po nim odczyt sam z siebie —
//! pętla zamykałaby się w miejscu, które miało ją przerwać.
//!
//! Zapisu tak nie ograniczamy i nie wolno tego zmienić. Zapis, który przerwie
//! się w połowie, ma prawo zostawić część bajtów w gnieździe — a wtedy druga
//! strona czyta długość jednej ramki i ciało następnej. Zapisy idą do bufora
//! jądra i wracają od razu; ryzyko nie ma tu czego równoważyć.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Jak długo czekamy na dane, zanim podniesiemy głowę i sprawdzimy flagę.
///
/// Krócej niż karencja, z jaką okno czeka na sesję przy zamykaniu — inaczej
/// odpuszczałoby zawsze, zamiast tylko wtedy, gdy naprawdę jest na czym stać.
pub const POLL: Duration = Duration::from_millis(100);

/// Ile czekamy na zestawienie połączenia z odbiornikiem.
///
/// Rzecz dzieje się w sieci lokalnej. Domyślny limit systemowy liczy się
/// w dziesiątkach sekund, bo przewiduje drugi koniec świata; tutaj tak długie
/// czekanie znaczy tylko tyle, że pod tym adresem nic nie ma.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Kanał sterujący sesji.
///
/// Opakowuje gniazdo i flagę „pracuj". Czyta się z niego i pisze jak
/// do zwykłego `TcpStream` — z tą różnicą, że odczyt kończy się błędem
/// [`io::ErrorKind::ConnectionAborted`], gdy flaga opadnie.
pub struct Control {
    stream: TcpStream,
    running: Arc<AtomicBool>,
}

impl Control {
    /// Bierze pod opiekę gniazdo, które już jest połączone.
    pub fn adopt(stream: TcpStream, running: Arc<AtomicBool>) -> io::Result<Self> {
        // Gniazdo z nasłuchu dziedziczy po nim tryb nieblokujący, a w tym
        // trybie limit czasu jest ignorowany — pytalibyśmy gniazdo w kółko,
        // zjadając rdzeń na nic.
        stream.set_nonblocking(false)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(POLL))?;
        Ok(Self { stream, running })
    }

    /// Łączy się z odbiornikiem, nie dłużej niż [`CONNECT_TIMEOUT`].
    pub fn connect(addr: SocketAddr, running: Arc<AtomicBool>) -> io::Result<Self> {
        let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;
        Self::adopt(stream, running)
    }

    /// Drugi uchwyt do tego samego gniazda — dla wątku, który czyta
    /// statystyki, podczas gdy pętla główna nadaje.
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            stream: self.stream.try_clone()?,
            running: Arc::clone(&self.running),
        })
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.stream.peer_addr()
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.stream.local_addr()
    }
}

/// Czy błąd znaczy tylko tyle, że w tej chwili nic nie przyszło.
fn empty_handed(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

impl Read for Control {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match (&self.stream).read(buf) {
                Err(ref e) if empty_handed(e) => {
                    if !self.running.load(Ordering::Relaxed) {
                        return Err(io::Error::new(
                            io::ErrorKind::ConnectionAborted,
                            "sesja zatrzymana",
                        ));
                    }
                }
                other => return other,
            }
        }
    }
}

impl Write for Control {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&self.stream).write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        (&self.stream).flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;
    use std::net::TcpListener;
    use std::time::Instant;

    /// Para połączonych gniazd. Drugie oddajemy, żeby żyło — zamknięte
    /// zerwałoby połączenie i odczyt skończyłby się sam, bez naszego udziału.
    fn linked(running: &Arc<AtomicBool>) -> (Control, TcpStream) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();
        (Control::adopt(server, Arc::clone(running)).unwrap(), client)
    }

    /// Druga strona milczy — dokładnie tak wygląda maszyna, która połączyła
    /// się i czeka, aż ktoś przepisze kod parowania.
    #[test]
    fn a_silent_peer_does_not_hold_the_session() {
        let running = Arc::new(AtomicBool::new(true));
        let (mut control, _client) = linked(&running);

        let theirs = Arc::clone(&running);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            theirs.store(false, Ordering::Relaxed);
        });

        let start = Instant::now();
        let err = control.read(&mut [0u8; 16]).unwrap_err();
        // Nie `Interrupted`: po nim `read_exact` ponawia odczyt i pętla,
        // którą właśnie przerywamy, zamknęłaby się z powrotem.
        assert_eq!(err.kind(), ErrorKind::ConnectionAborted);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "odczyt trzymał sesję {:?}",
            start.elapsed()
        );
    }

    /// Limit czasu nie może zjadać danych: bajty rozłożone w czasie mają
    /// dojść w całości i we właściwej kolejności.
    #[test]
    fn a_slow_peer_still_gets_every_byte_through() {
        let running = Arc::new(AtomicBool::new(true));
        let (mut control, mut client) = linked(&running);

        std::thread::spawn(move || {
            for chunk in [b"raz".as_slice(), b"dwa", b"trzy"] {
                // Dłużej niż limit odczytu, żeby każda porcja trafiła
                // w osobne przebudzenie.
                std::thread::sleep(POLL * 2);
                client.write_all(chunk).unwrap();
            }
        });

        let mut got = [0u8; 10];
        control.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"razdwatrzy");
    }
}
