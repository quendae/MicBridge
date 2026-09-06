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

/// Jak nazywa się dziennik i gdzie leży.
const LOG_NAME: &str = "micbridge.log";

fn main() -> eframe::Result {
    let log = start_logging();

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
        Box::new(move |cc| Ok(Box::new(App::new(cc, log)))),
    )
}

/// Kieruje dziennik do pliku obok kluczy i zwraca jego ścieżkę.
///
/// Okno nie ma konsoli — w Windows odcina ją `windows_subsystem`, a w Linuksie
/// program uruchomiony z menu nie ma dokąd pisać. Wszystko, co program mówił
/// o sobie po drodze, przepadało więc dokładnie w tych przypadkach, w których
/// było potrzebne. Bez pliku nie ma sensu odsyłać nikogo „do dziennika”.
///
/// Plik zaczyna się od nowa przy każdym uruchomieniu: interesuje nas ten
/// przebieg, w którym coś nie zadziałało, a nie wszystkie od instalacji.
/// Gdy pliku nie da się otworzyć, zostaje standardowe wyjście — lepsze to
/// niż program, który nie startuje przez dziennik.
fn start_logging() -> Option<std::path::PathBuf> {
    // Pierwsza nazwa to nazwa binarki, nie skrzynki: `[[bin]] name` brzmi
    // „micbridge-gui”, więc tracing widzi cel `micbridge_gui`. Stało tu
    // „mb_gui” i przez to własne wpisy okna — te o braku ikony w zasobniku
    // czy o nieudanym autostarcie — nie przechodziły przez filtr wcale.
    let filter = || {
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            "micbridge_gui=info,mb_app=info,mb_audio=info,mb_engine=info,mb_net=info".into()
        })
    };

    let path = mb_net::config_dir().ok().map(|dir| dir.join(LOG_NAME));
    if let Some(path) = &path {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let opened = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path);
        if let Ok(file) = opened {
            tracing_subscriber::fmt()
                .with_env_filter(filter())
                .with_target(false)
                .with_ansi(false)
                .with_writer(move || file.try_clone().expect("uchwyt dziennika"))
                .init();
            tracing::info!(dziennik = %path.display(), "start");
            return Some(path.clone());
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter(filter())
        .with_target(false)
        .init();
    None
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
    /// Czy użytkownik sam ruszył pole „Do:".
    ///
    /// Póki nie ruszył, wyszukiwanie ma prawo wpisać tam to, co znalazło.
    /// Potem już nie: podmienianie komuś wyboru pod palcami co dziesięć
    /// sekund byłoby gorsze od braku podpowiedzi.
    target_touched: bool,

    /// Wpisywany kod parowania.
    code: String,
    autostart: bool,
    /// Ostatnie niepowodzenie z paska na dole — autostart, zapis języka albo
    /// zapomnienie parowania. Jedno miejsce wystarczy: naraz i tak wychodzi
    /// najwyżej jedno.
    footer_error: Option<String>,

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

    /// Gdzie leży dziennik. `None`, gdy nie dało się go otworzyć i wszystko
    /// idzie na standardowe wyjście.
    pub(crate) log: Option<std::path::PathBuf>,
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
    fn new(cc: &eframe::CreationContext<'_>, log: Option<std::path::PathBuf>) -> Self {
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
            target_touched: false,
            code: String::new(),
            autostart: autostart::enabled(),
            footer_error: None,
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
            log,
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

    /// Zapomina klucz jednej maszyny, żeby dało się sparować od nowa.
    ///
    /// Wystarczy po jednej stronie: druga dowie się o tym przy najbliższym
    /// połączeniu — bo o parowaniu decyduje to, czy *któraś* ze stron klucza
    /// nie ma, a nie to, czy nie mają go obie.
    fn forget_peer(&mut self, peer: &str) {
        self.footer_error = match mb_net::KeyStore::open().and_then(|mut s| s.forget(peer)) {
            Ok(_) => None,
            Err(e) => {
                tracing::warn!(peer, error = %e, "nie mogę zapomnieć parowania");
                Some(format!("{e}"))
            }
        };
        self.refresh_paired(true);
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

    /// Wpisuje znalezioną maszynę do „Do:", dopóki nikt nie wybrał czego innego.
    ///
    /// Bez tego jedynym śladem, że wyszukiwanie w ogóle coś dało, było
    /// rozwinięcie listy: zamknięty wybór pokazywał „jedyny w sieci"
    /// niezależnie od tego, czy w sieci ktoś był, czy nie było nikogo.
    ///
    /// Tylko przy jednym znalezionym. Przy kilku wybór należy do użytkownika
    /// — podstawienie pierwszego z brzegu byłoby zgadywaniem, a nadajnik
    /// poszedłby wtedy do kogoś innego, niż ktokolwiek prosił.
    fn autofill_target(&mut self) {
        if self.target_touched || self.target != Target::Auto {
            return;
        }
        let name = {
            let mut usable = self.peers.iter().filter(|p| p.compatible());
            match (usable.next(), usable.next()) {
                (Some(only), None) => only.name.clone(),
                _ => return,
            }
        };
        self.target = Target::Named(name);
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
                self.autofill_target();
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
                // Wersja stoi przy nazwie, bo najczęściej szuka się jej wtedy,
                // gdy dwie maszyny nie chcą ze sobą gadać — a wtedy pierwsze
                // pytanie brzmi, czy obie są na tym samym.
                ui.label(
                    egui::RichText::new(concat!("v", env!("CARGO_PKG_VERSION")))
                        .weak()
                        .size(13.0),
                );
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
        // Ikona musi zejść z zasobnika, zanim proces zniknie. Windows nie
        // sprząta po programie, który się nie pożegnał — zostawia obrazek,
        // który wygląda jak działający program, a nie odpowiada na nic,
        // dopóki ktoś nie najedzie na niego myszą.
        self.tray = None;
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
                // Sesja czekająca na przepisanie kodu stoi na zmiennej
                // warunkowej, nie na gnieździe — flaga sama jej nie ruszy.
                self.state.cancel_code();
                // Ikona znika od razu, razem z oknem — bo to jedyne, co po
                // programie widać, a między tą klatką a końcem procesu sesje
                // mają jeszcze chwilę na rozejście się.
                self.tray = None;
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
