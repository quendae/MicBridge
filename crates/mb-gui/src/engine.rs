//! Uruchamianie i zatrzymywanie sesji z poziomu okna.
//!
//! Sesja jest blokująca, więc mieszka na własnym wątku. Okno steruje nią przez
//! flagę: podniesienie startuje wątek, opuszczenie prosi go o wyjście, a on
//! sam sprząta po sobie i melduje, że skończył.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eframe::egui;

use crate::state::{GuiReporter, Handle, Which};

/// Ile czekamy przy zamykaniu, aż sesja rozejdzie się po dobroci.
///
/// Pętle sesji sprawdzają flagę co sto milisekund, więc zwykle wystarcza
/// znacznie mniej.
const GRACE: Duration = Duration::from_millis(300);

/// Jak często zaglądamy, czy sesja już się skończyła.
const GRACE_POLL: Duration = Duration::from_millis(5);

/// Jedna strona pracy: odbieranie albo wysyłanie.
pub struct Engine {
    which: Which,
    running: Option<Arc<AtomicBool>>,
    thread: Option<JoinHandle<()>>,
}

impl Engine {
    pub fn new(which: Which) -> Self {
        Self {
            which,
            running: None,
            thread: None,
        }
    }

    pub fn is_running(&self) -> bool {
        self.thread.is_some()
    }

    /// Startuje sesję. `work` dostaje flagę zatrzymania i raportowanie.
    pub fn start<F>(&mut self, state: &Handle, ctx: &egui::Context, work: F)
    where
        F: FnOnce(&dyn mb_app::Reporter, Arc<AtomicBool>) -> anyhow::Result<()> + Send + 'static,
    {
        if self.is_running() {
            return;
        }
        let running = Arc::new(AtomicBool::new(true));
        let reporter = GuiReporter {
            state: Arc::clone(state),
            which: self.which,
            repaint: ctx.clone(),
        };
        let flag = Arc::clone(&running);
        let state = Arc::clone(state);
        let which = self.which;
        let ctx = ctx.clone();

        self.running = Some(running);
        self.thread = Some(std::thread::spawn(move || {
            let outcome = work(&reporter, flag);
            if let Ok(mut shared) = state.shared.lock() {
                let side = match which {
                    Which::Recv => &mut shared.recv,
                    Which::Send => &mut shared.send,
                };
                side.forget_session();
                side.running = false;
                match outcome {
                    Ok(()) => side.error = None,
                    Err(e) => {
                        // Błąd zostaje na ekranie, dopóki użytkownik czegoś nie
                        // zmieni — sesja właśnie znika, nie ma kto go powtórzyć.
                        tracing::error!(error = %e, "sesja zakończona błędem");
                        side.error = Some(format!("{e}"));
                        side.wanted = false;
                    }
                }
            }
            ctx.request_repaint();
        }));
    }

    /// Prosi sesję o zakończenie. Nie czeka — okno ma zostać responsywne.
    pub fn stop(&mut self) {
        if let Some(running) = &self.running {
            running.store(false, Ordering::Relaxed);
        }
    }

    /// Sprząta po wątku, który już się zatrzymał.
    pub fn reap(&mut self) {
        if self.thread.as_ref().is_some_and(JoinHandle::is_finished) {
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            self.running = None;
        }
    }
}

impl Drop for Engine {
    /// Prosi sesję o wyjście i czeka na nią — ale tylko chwilę.
    ///
    /// Zwykle sesja kończy się natychmiast: jej pętle zaglądają na flagę parę
    /// razy na sekundę. Ale nie wszystkie tam stoją. Sesja czekająca na
    /// przedstawienie się drugiej strony albo na kod parowania siedzi na
    /// blokującym czytaniu z gniazda i wróci dopiero, gdy tamta strona się
    /// odezwie — a maszyna, która właśnie się paruje, potrafi milczeć tyle,
    /// ile człowiek przepisuje kod.
    ///
    /// Czekanie bez końca zawieszało cały program: `Zakończ` zamykało okno,
    /// pętla zdarzeń kończyła się, a proces stawał tutaj — z martwą ikoną
    /// w zasobniku, bo do jej sprzątnięcia nigdy nie dochodziło. Lepiej
    /// zostawić niedobitka: proces i tak zaraz znika, a wątek ginie razem
    /// z nim; gniazda i urządzenie dźwiękowe zamyka system.
    fn drop(&mut self) {
        self.stop();
        let Some(thread) = self.thread.take() else {
            return;
        };
        let deadline = Instant::now() + GRACE;
        while !thread.is_finished() {
            if Instant::now() >= deadline {
                tracing::warn!(
                    which = ?self.which,
                    "sesja nie zdążyła się zamknąć — zostawiam ją procesowi"
                );
                return;
            }
            std::thread::sleep(GRACE_POLL);
        }
        let _ = thread.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::State;

    /// Sesja, która nie ogląda się na flagę — tak zachowuje się ta, która stoi
    /// na blokującym czytaniu z gniazda i czeka, aż druga strona się odezwie.
    /// Zamknięcie programu nie może od niej zależeć.
    #[test]
    fn quitting_does_not_wait_for_a_deaf_session() {
        let state: Handle = Arc::new(State::default());
        let ctx = egui::Context::default();
        let mut engine = Engine::new(Which::Recv);

        let release = Arc::new(AtomicBool::new(false));
        let theirs = Arc::clone(&release);
        engine.start(&state, &ctx, move |_ui, _running| {
            while !theirs.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(())
        });

        let start = Instant::now();
        drop(engine);
        let waited = start.elapsed();

        // Wątek nie ma po co zostawać do końca testów.
        release.store(true, Ordering::Relaxed);

        assert!(
            waited < GRACE * 4,
            "zamknięcie czekało {waited:?}, a miało odpuścić po {GRACE:?}"
        );
    }
}
