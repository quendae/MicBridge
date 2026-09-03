//! Warstwa raportowania: jedno miejsce, przez które sesja mówi o sobie.
//!
//! Terminal i okno potrzebują tego samego — kto się połączył, jakie jest
//! opóźnienie, ile ginie — tylko podają to inaczej. Sesje nie wiedzą, która
//! z nich patrzy; wołają tu i tyle.
//!
//! Kod parowania też idzie tędy, bo w terminalu wpisuje się go na klawiaturze,
//! a w oknie w polu tekstowym, i sesja nie ma powodu znać różnicy.

use std::io::{BufRead, Write};

use anyhow::{bail, Context, Result};

/// Stan sesji po stronie odbierającej, raz na sekundę.
#[derive(Debug, Clone, Default)]
pub struct RecvStatus {
    /// Głębokość bufora jitter — poduszka na kaprysy sieci.
    pub buffer_ms: f32,
    /// Zapas przed samą kartą; dyktuje go rozmiar porcji, o jaką prosi.
    pub ring_ms: f32,
    /// Do jakiej poduszki dążymy.
    pub target_ms: f32,
    pub loss_pct: f32,
    pub recovered: u64,
    pub jitter_ms: f32,
    pub drift_pct: f64,
    /// Próbki, których zabrakło karcie w ostatniej sekundzie.
    pub starved: u64,
    /// Ramki wyrzucone, bo bufor był pełny.
    pub dropped: u64,
    /// Ujście nie prosi o próbki: nikt jeszcze nie słucha.
    pub idle: bool,
}

impl RecvStatus {
    /// Całe opóźnienie, jakie dokładamy po stronie odbiorczej.
    pub fn latency_ms(&self) -> f32 {
        self.buffer_ms + self.ring_ms
    }
}

/// Stan sesji po stronie nadającej, raz na sekundę.
#[derive(Debug, Clone, Default)]
pub struct SendStatus {
    pub frames: u64,
    pub kbps: f32,
    pub peak_dbfs: f32,
    /// Straty zgłoszone przez odbiornik — nadajnik sam ich nie widzi.
    pub loss_pct: f32,
    /// Opóźnienie zgłoszone przez odbiornik.
    pub latency_ms: f32,
    pub jitter_ms: f32,
    /// Ile procent strat zakłada koder przy liczeniu korekcji.
    pub fec_pct: u8,
    /// Próbki zgubione, bo wątek sieciowy nie nadążył.
    pub overruns: u64,
    /// Pakiety zgubione celowo w trybie diagnostycznym.
    pub dropped_on_purpose: u64,
}

/// Odbiorca meldunków z sesji.
pub trait Reporter: Send + Sync {
    /// Wolny tekst — rzeczy, które zdarzają się raz.
    fn line(&self, text: &str);

    fn recv_status(&self, status: &RecvStatus);
    fn send_status(&self, status: &SendStatus);

    /// Odbiornik pokazuje kod parowania.
    fn show_code(&self, peer: &str, code: &str);

    /// Nadajnik prosi o przepisanie kodu z drugiego ekranu.
    fn ask_code(&self, peer: &str) -> Result<String>;

    /// Sesja stanęła: kto po drugiej stronie i gdzie ląduje dźwięk.
    fn connected(&self, peer: &str, detail: &str);
}

/// Raportowanie do terminala.
pub struct Console;

impl Reporter for Console {
    fn line(&self, text: &str) {
        println!("{text}");
    }

    fn recv_status(&self, s: &RecvStatus) {
        if s.idle {
            println!("  czekam — nikt jeszcze nie słucha");
            return;
        }
        println!(
            "  bufor {:>3.0}+{:>2.0} ms   cel {:>3.0}   strat {:>4.1}% (FEC {})   \
             jitter {:>4.1} ms   dryf {:+.3}%{}",
            s.buffer_ms,
            s.ring_ms,
            s.target_ms,
            s.loss_pct,
            s.recovered,
            s.jitter_ms,
            s.drift_pct * 100.0,
            // Przepełnienie bufora nie liczy się jako strata pakietu, ale
            // wyrzucone ramki słychać tak samo — bez tego licznika stan
            // „strat 0,0%” towarzyszył rwącemu się dźwiękowi.
            match (s.starved, s.dropped) {
                (0, 0) => String::new(),
                (n, 0) => format!("   NIEDOMIAR {n}"),
                (0, n) => format!("   WYRZUCONO {n} ramek (bufor pełny)"),
                (a, b) => format!("   NIEDOMIAR {a}   WYRZUCONO {b}"),
            }
        );
    }

    fn send_status(&self, s: &SendStatus) {
        let mut tail = format!("   FEC na {}% strat", s.fec_pct);
        if s.overruns > 0 {
            tail.push_str(&format!("   zgubiono {} próbek", s.overruns));
        }
        if s.dropped_on_purpose > 0 {
            tail.push_str(&format!("   zgubiono celowo {}", s.dropped_on_purpose));
        }
        println!(
            "  {:>8} ramek  {:>6.1} kbps   szczyt {:>6.1} dBFS{tail}",
            s.frames, s.kbps, s.peak_dbfs
        );
    }

    fn show_code(&self, peer: &str, code: &str) {
        println!("\n„{peer}” prosi o sparowanie.");
        println!("  KOD: {}", mb_net::pair::format_code(code));
        println!("  Przepisz go na drugiej maszynie.");
    }

    fn ask_code(&self, peer: &str) -> Result<String> {
        println!("\n„{peer}” nie jest jeszcze sparowany.");
        println!("Na jego ekranie pojawił się sześciocyfrowy kod.");
        print!("Przepisz go tutaj: ");
        std::io::stdout().flush().ok();

        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .context("nie mogę odczytać kodu")?;
        check_code(&mb_net::pair::normalize_code(&line))
    }

    fn connected(&self, peer: &str, detail: &str) {
        println!("\n{peer} → {detail}");
    }
}

/// Kod podany z góry — dla skryptów i usług, gdzie nie ma komu wpisywać.
pub struct FixedCode<R: Reporter> {
    pub inner: R,
    pub code: String,
}

impl<R: Reporter> Reporter for FixedCode<R> {
    fn line(&self, text: &str) {
        self.inner.line(text);
    }
    fn recv_status(&self, status: &RecvStatus) {
        self.inner.recv_status(status);
    }
    fn send_status(&self, status: &SendStatus) {
        self.inner.send_status(status);
    }
    fn show_code(&self, peer: &str, code: &str) {
        self.inner.show_code(peer, code);
    }
    fn ask_code(&self, _peer: &str) -> Result<String> {
        check_code(&mb_net::pair::normalize_code(&self.code))
    }
    fn connected(&self, peer: &str, detail: &str) {
        self.inner.connected(peer, detail);
    }
}

/// Wspólne sprawdzenie długości — literówka ma wyjść tutaj, a nie po drugiej
/// stronie jako „parowanie odrzucone”.
pub fn check_code(code: &str) -> Result<String> {
    if code.len() != mb_net::pair::CODE_DIGITS {
        bail!(
            "kod ma {} cyfr, oczekuję {}",
            code.len(),
            mb_net::pair::CODE_DIGITS
        );
    }
    Ok(code.to_string())
}
