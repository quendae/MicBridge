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
mod state;
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
            .with_inner_size([680.0, 760.0])
            .with_min_inner_size([520.0, 520.0])
            .with_title("MicBridge"),
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
        cc.egui_ctx.set_zoom_factor(1.1);
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
        };
        app.reload_devices();
        app
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

        // Sesje meldują co sekundę, więc okno musi samo wracać do życia —
        // bez tego stan zamarłby do najbliższego ruchu myszą.
        ctx.request_repaint_after(Duration::from_millis(500));

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

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.pairing_ui(ui);
                self.recv_ui(ui, ctx);
                ui.add_space(14.0);
                self.send_ui(ui, ctx);
                ui.add_space(14.0);
                self.options_ui(ui);
            });
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.recv.stop();
        self.send.stop();
    }
}

fn names(dir: mb_audio::Direction) -> Vec<String> {
    mb_audio::list(dir)
        .map(|list| list.into_iter().map(|d| d.name).collect())
        .unwrap_or_default()
}
