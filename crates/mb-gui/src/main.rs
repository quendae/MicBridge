//! MicBridge — okno.
//!
//! Ta sama biblioteka co w terminalu, tylko pokazana. Dwie połowy: odbieranie
//! i wysyłanie. Każdą włącza się przełącznikiem, a to, co się dzieje, widać
//! w liczbach i na wykresach — opóźnienie i straty po obu stronach, bo problem
//! rzadko wygląda tak samo z obu końców łącza.

// W Windows bez tego okno ciągnie za sobą konsolę.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod engine;
mod icon;
mod state;
mod tray;
mod wake;
mod widgets;

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;

use engine::Engine;
use state::{Handle, State, Which};

/// Jak często odświeżamy listę maszyn widocznych w sieci.
const REFRESH_PEERS: Duration = Duration::from_secs(10);

fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mb_gui=info,mb_app=info,mb_audio=info,mb_net=info".into()),
        )
        .with_target(false)
        .init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([680.0, 620.0])
            .with_min_inner_size([460.0, 460.0])
            .with_title("MicBridge")
            .with_icon(icon::window()),
        ..Default::default()
    };

    eframe::run_native(
        "MicBridge",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

struct App {
    state: Handle,
    recv: Engine,
    send: Engine,

    // Ustawienia odbierania.
    sink: String,
    buffer_ms: u32,
    adaptive: bool,
    announce: bool,

    // Ustawienia wysyłania.
    device: String,
    target: Target,

    /// Wpisywany kod parowania.
    code: String,
    autostart: bool,
    autostart_error: Option<String>,

    sinks: Vec<String>,
    mics: Vec<String>,
    peers: Vec<mb_net::Peer>,
    peers_refreshed: Option<Instant>,
    /// Wyszukiwanie chodzi na osobnym wątku — trwa sekundy, a okno ma być żywe.
    peers_pending: Option<std::sync::mpsc::Receiver<Vec<mb_net::Peer>>>,

    /// Ikona w zasobniku. `None`, gdy system jej nie daje — wtedy zamknięcie
    /// okna po prostu kończy program, bo inaczej nie byłoby jak do niego wrócić.
    tray: Option<tray::Tray>,
    /// Przywraca okno na ekran, gdy schowane przestaje dostawać klatki.
    waker: wake::Waker,
    /// Czy okno jest schowane w zasobniku.
    hidden: bool,
    /// Czy program się właśnie kończy.
    ///
    /// Prośba o zamknięcie wraca do nas klatkę później jako zwykłe zamknięcie
    /// okna — a to chowamy do zasobnika. Bez tej pamięci program odwoływałby
    /// własne wyjście i nie dałoby się go wyłączyć inaczej niż z zewnątrz.
    quitting: bool,

    /// Nazwy sparowanych maszyn. Trzymane, bo lista siedzi w pliku, a klatek
    /// jest kilka na sekundę — czytanie go za każdym razem byłoby zaglądaniem
    /// na dysk bez powodu.
    paired: Vec<String>,
    paired_at: Option<Instant>,
}

/// Do kogo nadajemy.
#[derive(PartialEq, Eq, Clone)]
enum Target {
    /// Jedyny odbiornik w sieci — niech program sam go znajdzie.
    Auto,
    /// Wskazany z listy albo wpisany ręcznie.
    Named(String),
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let waker = wake::Waker::new(cc);
        let mut app = Self {
            state: Arc::new(State::default()),
            recv: Engine::new(Which::Recv),
            send: Engine::new(Which::Send),
            sink: "auto".into(),
            buffer_ms: 30,
            adaptive: true,
            announce: true,
            device: "default".into(),
            target: Target::Auto,
            code: String::new(),
            autostart: autostart::enabled(),
            autostart_error: None,
            sinks: Vec::new(),
            mics: Vec::new(),
            peers: Vec::new(),
            peers_refreshed: None,
            peers_pending: None,
            tray: match tray::Tray::new(&waker) {
                Ok(t) => Some(t),
                Err(e) => {
                    tracing::warn!(error = %e, "brak ikony w zasobniku");
                    None
                }
            },
            waker,
            hidden: false,
            quitting: false,
            paired: Vec::new(),
            paired_at: None,
        };
        app.reload_devices();
        app.refresh_paired(true);
        app
    }

    /// Odświeża listę sparowanych maszyn, ale nie częściej niż co sekundę.
    fn refresh_paired(&mut self, force: bool) {
        let stale = self
            .paired_at
            .is_none_or(|t| t.elapsed() > Duration::from_secs(1));
        if !force && !stale {
            return;
        }
        self.paired_at = Some(Instant::now());
        self.paired = mb_net::KeyStore::open()
            .map(|store| store.peers().map(str::to_owned).collect())
            .unwrap_or_default();
    }

    fn reload_devices(&mut self) {
        self.mics = names(mb_audio::Direction::Input);
        self.sinks = names(mb_audio::Direction::Output);
    }

    /// Zaczyna wyszukiwanie w sieci, jeśli nie trwa i minęło dość czasu.
    fn refresh_peers(&mut self, force: bool) {
        if self.peers_pending.is_some() {
            return;
        }
        let stale = self
            .peers_refreshed
            .is_none_or(|t| t.elapsed() > REFRESH_PEERS);
        if !force && !stale {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.peers_pending = Some(rx);
        std::thread::spawn(move || {
            let found = mb_net::browse(Duration::from_millis(1500)).unwrap_or_default();
            let _ = tx.send(found);
        });
    }

    fn collect_peers(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.peers_pending else {
            return;
        };
        match rx.try_recv() {
            Ok(found) => {
                self.peers = found;
                self.peers_refreshed = Some(Instant::now());
                self.peers_pending = None;
                ctx.request_repaint();
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.peers_pending = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }

    fn start_recv(&mut self, ctx: &egui::Context) {
        let opts = mb_app::recv::Options {
            listen: format!("0.0.0.0:{}", mb_proto::CONTROL_PORT),
            sink: self.sink.clone(),
            buffer_ms: self.buffer_ms,
            adaptive: self.adaptive,
            announce: self.announce,
        };
        self.recv.start(&self.state, ctx, move |ui, running| {
            mb_app::recv::run(&opts, ui, running)
        });
    }

    fn start_send(&mut self, ctx: &egui::Context) {
        let opts = mb_app::send::Options {
            to: match &self.target {
                Target::Auto => None,
                Target::Named(name) => Some(name.clone()),
            },
            device: self.device.clone(),
            ..Default::default()
        };
        self.send.start(&self.state, ctx, move |ui, running| {
            mb_app::send::run(&opts, ui, running)
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.recv.reap();
        self.send.reap();
        self.collect_peers(ctx);
        self.refresh_paired(false);
        if self.handle_tray(ctx) {
            return;
        }
        self.handle_close(ctx);

        // Sesje meldują co sekundę, więc okno musi samo wracać do życia — bez
        // tego stan zamarłby do najbliższego ruchu myszą. Schowanego nie ma
        // sensu budzić: system i tak nie da mu klatki, a nie ma tam nic do
        // pokazania. Kliknięcie w ikonę obudzi je wtedy inną drogą (`wake.rs`).
        if !self.hidden {
            ctx.request_repaint_after(Duration::from_millis(500));
        }

        egui::TopBottomPanel::top("naglowek").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading("MicBridge");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(mb_net::hostname()).weak().size(13.0));
                });
            });
            ui.add_space(6.0);
        });

        egui::TopBottomPanel::bottom("stopka").show(ctx, |ui| {
            ui.add_space(4.0);
            self.footer_ui(ui);
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.pairing_ui(ui);
                self.recv_ui(ui, ctx);
                ui.add_space(14.0);
                self.send_ui(ui, ctx);
            });
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.recv.stop();
        self.send.stop();
    }
}

impl App {
    /// Zamknięcie okna chowa program do zasobnika zamiast go kończyć.
    ///
    /// Sesja potrafi grać godzinami; zamknięcie okna nie jest prośbą o jej
    /// przerwanie. Wyjście jest w menu ikony — a gdy ikony nie ma, zamknięcie
    /// znaczy to, co zwykle.
    fn handle_close(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        // Chowamy się tylko tam, skąd umiemy wrócić. Gdzie indziej zamknięcie
        // znaczy to, co zwykle — lepsze niż program bez okna i bez wyjścia.
        if self.quitting || self.tray.is_none() || !self.waker.can_restore() {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        self.hidden = true;
    }

    /// Zwraca `true`, gdy program właśnie się kończy i nie ma po co rysować.
    fn handle_tray(&mut self, ctx: &egui::Context) -> bool {
        let Some(tray) = &self.tray else {
            return false;
        };
        match tray.poll() {
            Some(tray::Action::Show) => {
                // Okno już wróciło na ekran — zrobił to budzik, zanim ta
                // klatka w ogóle powstała. Zostaje uzgodnić stan, bo eframe
                // wciąż uważa je za schowane.
                self.hidden = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                false
            }
            Some(tray::Action::Quit) => {
                self.quitting = true;
                self.recv.stop();
                self.send.stop();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                true
            }
            None => false,
        }
    }
}

fn names(dir: mb_audio::Direction) -> Vec<String> {
    mb_audio::list(dir)
        .map(|list| list.into_iter().map(|d| d.name).collect())
        .unwrap_or_default()
}
