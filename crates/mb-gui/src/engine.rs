//! Uruchamianie i zatrzymywanie sesji z poziomu okna.
//!
//! Sesja jest blokująca, więc mieszka na własnym wątku. Okno steruje nią przez
//! flagę: podniesienie startuje wątek, opuszczenie prosi go o wyjście, a on
//! sam sprząta po sobie i melduje, że skończył.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use eframe::egui;

use crate::state::{GuiReporter, Handle, Which};

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
    fn drop(&mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
