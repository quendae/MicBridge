//! Stan, który okno pokazuje, i most między nim a sesjami.
//!
//! Sesje pracują na własnych wątkach i nic nie wiedzą o oknie — meldują przez
//! [`mb_app::Reporter`]. Tu te meldunki lądują w strukturze, którą okno czyta
//! przy każdym odrysowaniu.
//!
//! Jedno miejsce wymaga ruchu w drugą stronę: kod parowania. Sesja zatrzymuje
//! się i czeka, aż użytkownik go wpisze — w terminalu robi to `read_line`,
//! tutaj zmienna warunkowa i pole tekstowe.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use anyhow::{bail, Result};
use eframe::egui;
use mb_app::ui::{RecvStatus, Reporter, SendStatus};
use mb_i18n::{t, t1, t2, Key as K};

/// Ile pomiarów trzyma wykres. Przy jednym na sekundę to dwie minuty — dość,
/// żeby zobaczyć, że coś się psuje, i za mało, żeby zjadło pamięć.
pub const HISTORY: usize = 120;

/// Jak długo czekamy na wpisanie kodu, zanim uznamy, że nikt nie patrzy.
const CODE_TIMEOUT: Duration = Duration::from_secs(180);

/// Przebieg jednej wielkości w czasie.
#[derive(Default)]
pub struct Series {
    pub points: VecDeque<f32>,
}

impl Series {
    pub fn push(&mut self, v: f32) {
        if self.points.len() == HISTORY {
            self.points.pop_front();
        }
        self.points.push_back(v);
    }

    pub fn last(&self) -> Option<f32> {
        self.points.back().copied()
    }

    /// Największa wartość albo podana podłoga — wykres o zerowej wysokości nie
    /// mówi nic, a skala skacząca przy każdej próbce męczy oko.
    pub fn ceiling(&self, floor: f32) -> f32 {
        self.points.iter().copied().fold(floor, f32::max)
    }
}

/// Co widać w jednej połowie okna.
#[derive(Default)]
pub struct Side {
    /// Czy sesja ma działać. Ustawia okno, czyta wątek sesji.
    pub wanted: bool,
    /// Czy wątek faktycznie chodzi.
    pub running: bool,
    /// Z kim i gdzie — jedna linijka pod przełącznikiem.
    pub peer: Option<String>,
    pub detail: String,
    /// Ostatnie kilka zdań: co się stało, czego brakuje.
    pub log: VecDeque<String>,
    pub latency: Series,
    pub loss: Series,
    /// Liczby, których nie ma sensu rysować.
    pub numbers: Vec<(String, String)>,
    /// Ostatni błąd, dopóki użytkownik czegoś nie zmieni.
    pub error: Option<String>,
}

impl Side {
    fn note(&mut self, text: &str) {
        if self.log.len() == 12 {
            self.log.pop_front();
        }
        self.log.push_back(text.to_string());
    }

    /// Sesja stanęła — wszystko, co pokazywaliśmy, przestaje być prawdą.
    pub fn forget_session(&mut self) {
        self.peer = None;
        self.detail.clear();
        self.numbers.clear();
        self.latency.points.clear();
        self.loss.points.clear();
    }
}

/// Prośba o kod parowania, czekająca na odpowiedź z okna.
#[derive(Default)]
pub struct CodePrompt {
    /// Kto pyta. `None` znaczy, że nikt nie czeka.
    pub peer: Option<String>,
    /// Wpisane cyfry; okno je uzupełnia, sesja zabiera.
    answer: Option<String>,
    /// Użytkownik zrezygnował.
    cancelled: bool,
}

/// Kod pokazany przez nasz odbiornik drugiej maszynie.
#[derive(Default)]
pub struct ShownCode {
    pub peer: String,
    pub code: String,
}

#[derive(Default)]
pub struct Shared {
    pub recv: Side,
    pub send: Side,
    /// Kod, który pokazujemy komuś, kto chce się z nami sparować.
    pub shown_code: Option<ShownCode>,
}

/// Wszystko, co okno i sesje mają wspólnego.
///
/// Prośba o kod stoi obok `shared`, a nie w środku: sesja czeka na niej
/// z zamkiem, a okno musi w tym czasie normalnie czytać resztę stanu, żeby
/// się rysować.
#[derive(Default)]
pub struct State {
    pub shared: Mutex<Shared>,
    prompt: Mutex<CodePrompt>,
    prompt_ready: Condvar,
}

pub type Handle = Arc<State>;

/// Meldunki jednej strony trafiają w jedno miejsce w [`Shared`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Which {
    Recv,
    Send,
}

/// Raportowanie do okna.
pub struct GuiReporter {
    pub state: Handle,
    pub which: Which,
    /// Bez tego okno odrysowałoby się dopiero przy ruchu myszą.
    pub repaint: egui::Context,
}

impl GuiReporter {
    fn with<R>(&self, f: impl FnOnce(&mut Shared) -> R) -> Option<R> {
        let mut guard = self.state.shared.lock().ok()?;
        let out = f(&mut guard);
        drop(guard);
        self.repaint.request_repaint();
        Some(out)
    }

    fn side<'a>(&self, shared: &'a mut Shared) -> &'a mut Side {
        match self.which {
            Which::Recv => &mut shared.recv,
            Which::Send => &mut shared.send,
        }
    }
}

impl Reporter for GuiReporter {
    fn line(&self, text: &str) {
        tracing::info!(%text);
        self.with(|s| self.side(s).note(text));
    }

    fn recv_status(&self, status: &RecvStatus) {
        self.with(|s| {
            let side = self.side(s);
            side.latency.push(status.latency_ms());
            side.loss.push(status.loss_pct);
            side.numbers = vec![
                (
                    t(K::NumCushion).into(),
                    format!("{:.0} ms", status.buffer_ms),
                ),
                (t(K::NumCard).into(), format!("{:.0} ms", status.ring_ms)),
                (
                    t(K::NumJitter).into(),
                    format!("{:.1} ms", status.jitter_ms),
                ),
                (t(K::NumFec).into(), t1(K::FramesN, status.recovered)),
                (
                    t(K::NumDrift).into(),
                    format!("{:+.2}%", status.drift_pct * 100.0),
                ),
            ];
            if status.dropped > 0 {
                side.numbers
                    .push((t(K::NumDropped).into(), t1(K::FramesN, status.dropped)));
            }
            if status.starved > 0 {
                side.numbers
                    .push((t(K::NumStarved).into(), t1(K::SamplesN, status.starved)));
            }
            side.detail = if status.idle {
                t(K::NobodyListening).into()
            } else {
                t2(
                    K::LatencyLoss,
                    format!("{:.0}", status.latency_ms()),
                    format!("{:.1}", status.loss_pct),
                )
            };
        });
    }

    fn send_status(&self, status: &SendStatus) {
        self.with(|s| {
            let side = self.side(s);
            // Nadajnik nie ma jak zmierzyć opóźnienia sam — pokazuje to, co
            // zgłasza odbiornik, więc obie strony patrzą na tę samą liczbę.
            side.latency.push(status.latency_ms);
            side.loss.push(status.loss_pct);
            side.numbers = vec![
                (t(K::NumBitrate).into(), format!("{:.0} kbps", status.kbps)),
                (
                    t(K::NumPeak).into(),
                    format!("{:.0} dBFS", status.peak_dbfs),
                ),
                (
                    t(K::NumJitter).into(),
                    format!("{:.1} ms", status.jitter_ms),
                ),
                (t(K::NumFecFor).into(), t1(K::PctLoss, status.fec_pct)),
                (t(K::NumFrames).into(), format!("{}", status.frames)),
            ];
            if status.overruns > 0 {
                side.numbers
                    .push((t(K::NumLost).into(), t1(K::SamplesN, status.overruns)));
            }
            side.detail = t2(
                K::LatencyLoss,
                format!("{:.0}", status.latency_ms),
                format!("{:.1}", status.loss_pct),
            );
        });
    }

    fn show_code(&self, peer: &str, code: &str) {
        self.with(|s| {
            s.shown_code = Some(ShownCode {
                peer: peer.to_string(),
                code: code.to_string(),
            });
        });
    }

    fn ask_code(&self, peer: &str) -> Result<String> {
        // Okno pokaże pole; ten wątek stoi, aż coś w nie wpadnie.
        let mut prompt = self.state.prompt.lock().map_err(zajety)?;
        prompt.peer = Some(peer.to_string());
        prompt.answer = None;
        prompt.cancelled = false;
        self.repaint.request_repaint();

        loop {
            if prompt.cancelled {
                prompt.peer = None;
                bail!("{}", t(K::ErrPairingCancelled));
            }
            if let Some(code) = prompt.answer.take() {
                prompt.peer = None;
                return mb_app::ui::check_code(&mb_net::pair::normalize_code(&code));
            }
            let (next, timeout) = self
                .state
                .prompt_ready
                .wait_timeout(prompt, CODE_TIMEOUT)
                .map_err(zajety)?;
            prompt = next;
            if timeout.timed_out() {
                prompt.peer = None;
                bail!("{}", t(K::ErrNobodyTypedCode));
            }
        }
    }

    fn connected(&self, peer: &str, detail: &str) {
        self.with(|s| {
            let side = self.side(s);
            side.peer = Some(peer.to_string());
            side.error = None;
            side.note(&format!("{peer} → {detail}"));
        });
    }
}

fn zajety<E>(_: E) -> anyhow::Error {
    anyhow::anyhow!("stan współdzielony zajęty")
}

impl State {
    /// Podaje sesji kod wpisany w oknie.
    pub fn answer_code(&self, code: &str) {
        if let Ok(mut prompt) = self.prompt.lock() {
            prompt.answer = Some(code.to_string());
        }
        self.prompt_ready.notify_all();
    }

    /// Rezygnacja z parowania — sesja ma się poddać, a nie czekać w nieskończoność.
    pub fn cancel_code(&self) {
        if let Ok(mut prompt) = self.prompt.lock() {
            prompt.cancelled = true;
        }
        self.prompt_ready.notify_all();
    }

    /// Czy ktoś czeka na kod i od kogo.
    pub fn awaiting_code(&self) -> Option<String> {
        self.prompt.lock().ok()?.peer.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn reporter(which: Which) -> (Handle, GuiReporter) {
        let state: Handle = Arc::new(State::default());
        let ui = GuiReporter {
            state: Arc::clone(&state),
            which,
            repaint: egui::Context::default(),
        };
        (state, ui)
    }

    #[test]
    fn statuses_land_on_their_own_side() {
        let (state, ui) = reporter(Which::Recv);
        ui.recv_status(&RecvStatus {
            buffer_ms: 30.0,
            ring_ms: 48.0,
            loss_pct: 1.5,
            ..Default::default()
        });

        let shared = state.shared.lock().unwrap();
        assert_eq!(
            shared.recv.latency.last(),
            Some(78.0),
            "poduszka plus karta"
        );
        assert_eq!(shared.recv.loss.last(), Some(1.5));
        assert!(
            shared.send.latency.points.is_empty(),
            "druga strona nietknięta"
        );
    }

    #[test]
    fn the_history_stays_bounded() {
        let (state, ui) = reporter(Which::Send);
        for i in 0..HISTORY * 2 {
            ui.send_status(&SendStatus {
                latency_ms: i as f32,
                ..Default::default()
            });
        }
        let shared = state.shared.lock().unwrap();
        assert_eq!(shared.send.latency.points.len(), HISTORY);
        assert_eq!(
            shared.send.latency.last(),
            Some((HISTORY * 2 - 1) as f32),
            "zostają najnowsze, nie najstarsze"
        );
    }

    /// Sesja czeka na kod na osobnym wątku, a okno musi w tym czasie normalnie
    /// czytać stan. To był powód, dla którego prośba o kod stoi obok `shared`.
    #[test]
    fn waiting_for_a_code_does_not_block_the_window() {
        let (state, ui) = reporter(Which::Send);
        let asked = Arc::new(AtomicBool::new(false));

        let flag = Arc::clone(&asked);
        let session = std::thread::spawn(move || {
            flag.store(true, Ordering::Release);
            ui.ask_code("salon")
        });

        while !asked.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        // Okno rysuje się dalej: stan da się zamknąć i otworzyć.
        for _ in 0..100 {
            drop(state.shared.lock().unwrap());
            if state.awaiting_code().is_some() {
                break;
            }
            std::thread::yield_now();
        }
        assert_eq!(state.awaiting_code().as_deref(), Some("salon"));

        state.answer_code(" 482-193 ");
        assert_eq!(session.join().unwrap().unwrap(), "482193");
        assert!(
            state.awaiting_code().is_none(),
            "pytanie znika po odpowiedzi"
        );
    }

    #[test]
    fn giving_up_on_pairing_releases_the_session() {
        let (state, ui) = reporter(Which::Send);
        let session = std::thread::spawn(move || ui.ask_code("salon"));

        while state.awaiting_code().is_none() {
            std::thread::yield_now();
        }
        state.cancel_code();
        assert!(session.join().unwrap().is_err());
    }
}
