//! MicBridge — przenosi mikrofon z jednego komputera na drugi.
//!
//! Wersja dla terminala. Sesje żyją w `mb_app`; tutaj zostaje wiersz poleceń,
//! wypisywanie list i przechwycenie Ctrl-C. To samo `mb_app` napędza okno.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use mb_app::ui::{Console, FixedCode};
use mb_app::{recv, send};
use mb_audio::Direction;
use mb_proto::{CONTROL_PORT, PROTOCOL_VERSION, SAMPLE_RATE};

#[derive(Parser)]
#[command(
    name = "micbridge",
    version,
    about = "Mikrofon z jednego komputera na drugim",
    long_about = None
)]
struct Cli {
    /// Więcej logów (można powtórzyć: -vv).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Wypisz urządzenia audio widoczne w systemie.
    Devices,

    /// Sprawdź, czy ta maszyna jest gotowa wysyłać i odbierać.
    Doctor,

    /// Wypisz sparowane maszyny.
    Peers,

    /// Zapomnij parowanie z podaną maszyną.
    Forget {
        /// Nazwa z listy `peers`.
        #[arg(value_name = "NAZWA")]
        peer: String,
    },

    /// Pokaż odbiorniki widoczne w sieci lokalnej.
    Discover {
        /// Jak długo słuchać odpowiedzi.
        #[arg(long, default_value_t = 2500, value_name = "MS")]
        window_ms: u64,
    },

    /// Wysyłaj mikrofon do drugiego komputera.
    Send {
        /// Odbiornik: adres, `adres:port` albo nazwa z `micbridge discover`.
        /// Bez tej flagi szukamy go w sieci.
        #[arg(long, value_name = "ADRES")]
        to: Option<String>,

        /// Źródło: `default`, `@3` albo fragment nazwy, np. `yeti`.
        #[arg(long, default_value = "default", value_name = "URZĄDZENIE")]
        device: String,

        /// Wzmocnienie w decybelach, dodatnie podbija cichy mikrofon.
        #[arg(long, default_value_t = 0.0, value_name = "dB")]
        gain_db: f32,

        /// Przepływność Opusa. 24 kbps jest przezroczyste dla mowy.
        #[arg(long, default_value_t = 24_000, value_name = "bps")]
        bitrate: u32,

        /// Diagnostyka: gub celowo tyle procent pakietów, żeby sprawdzić FEC.
        #[arg(long, default_value_t = 0.0, value_name = "PCT")]
        drop_pct: f32,

        /// Kod parowania, gdy nie ma gdzie go wpisać (skrypty, usługi).
        #[arg(long, value_name = "CYFRY")]
        code: Option<String>,
    },

    /// Odbieraj mikrofon i wpuszczaj go w wirtualne urządzenie.
    Recv {
        /// Interfejs i port do nasłuchu.
        #[arg(long, default_value_t = format!("0.0.0.0:{CONTROL_PORT}"), value_name = "ADRES")]
        listen: String,

        /// Ujście: `auto` (Linux: własny mikrofon, Windows: wirtualny kabel),
        /// `virtual`, `device` albo fragment nazwy urządzenia.
        #[arg(long, default_value = "auto", value_name = "UJŚCIE")]
        sink: String,

        /// Startowa głębokość bufora jitter.
        #[arg(long, default_value_t = 30, value_name = "MS")]
        buffer_ms: u32,

        /// Nie dopasowuj poduszki do jakości łącza — trzymaj zadaną wartość.
        #[arg(long)]
        fixed_buffer: bool,

        /// Nie ogłaszaj się w sieci — druga strona poda adres ręcznie.
        #[arg(long)]
        no_announce: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    match cli.command {
        Command::Devices => list_devices(),
        Command::Discover { window_ms } => discover(window_ms),
        Command::Doctor => doctor(),
        Command::Peers => list_peers(),
        Command::Forget { peer } => forget_peer(&peer),
        Command::Send {
            to,
            device,
            gain_db,
            bitrate,
            drop_pct,
            code,
        } => send::run(
            &send::Options {
                to,
                device,
                gain_db,
                bitrate,
                drop_pct,
            },
            &*reporter(code),
            stop_on_ctrl_c()?,
        ),
        Command::Recv {
            listen,
            sink,
            buffer_ms,
            fixed_buffer,
            no_announce,
        } => recv::run(
            &recv::Options {
                listen,
                sink,
                buffer_ms,
                adaptive: !fixed_buffer,
                announce: !no_announce,
            },
            &Console,
            stop_on_ctrl_c()?,
        ),
    }
}

/// Kod parowania z wiersza poleceń zdejmuje pytanie z klawiatury — inaczej
/// wpisuje go człowiek.
fn reporter(code: Option<String>) -> Box<dyn mb_app::Reporter> {
    match code {
        Some(code) => Box::new(FixedCode {
            inner: Console,
            code,
        }),
        None => Box::new(Console),
    }
}

/// Flaga, którą Ctrl-C opuszcza. Sesje same nie dotykają sygnałów, bo w oknie
/// zatrzymuje je przycisk.
fn stop_on_ctrl_c() -> Result<Arc<AtomicBool>> {
    let running = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&running);
    ctrlc::set_handler(move || flag.store(false, Ordering::Relaxed))
        .context("nie mogę przechwycić Ctrl-C")?;
    println!("Ctrl-C kończy.");
    Ok(running)
}

fn init_tracing(verbose: u8) {
    let level = match verbose {
        0 => "mb_app=info,mb_audio=info,mb_engine=info",
        1 => "debug",
        _ => "trace",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| level.into()),
        )
        .with_target(false)
        .without_time()
        .init();
}

fn discover(window_ms: u64) -> Result<()> {
    println!("Szukam odbiorników przez {window_ms} ms…");
    let peers = mb_net::browse(std::time::Duration::from_millis(window_ms))?;

    if peers.is_empty() {
        println!("\nNic nie widzę.");
        println!("  • czy na drugiej maszynie działa `micbridge recv`?");
        println!("  • czy obie są w tej samej sieci?");
        println!("  • część routerów Wi-Fi blokuje ruch multicast między klientami;");
        println!("    wtedy zostaje `send --to 192.168.1.40`.");
        return Ok(());
    }

    println!("\nODBIORNIKI");
    for peer in &peers {
        let note = if peer.compatible() {
            String::new()
        } else {
            format!("  ← protokół {} zamiast {PROTOCOL_VERSION}", peer.version)
        };
        println!("  {:<24} {:<24}{note}", peer.name, peer.addr.to_string());
    }
    println!("\nWysyłanie: micbridge send --to \"{}\"", peers[0].name);
    Ok(())
}

/// Przegląd gotowości. Kod wyjścia niezerowy przy błędzie, żeby dało się
/// tego użyć w skrypcie albo w instalatorze.
fn doctor() -> Result<()> {
    let report = mb_app::doctor::check();
    println!("MicBridge — przegląd\n");
    for check in &report.checks {
        println!("{check}");
    }

    println!();
    match report.worst() {
        mb_app::doctor::Grade::Ok => println!("Wszystko na miejscu."),
        mb_app::doctor::Grade::Warn => {
            println!("Da się pracować, ale nie wszystkie role są dostępne.");
        }
        mb_app::doctor::Grade::Fail => {
            println!("Coś jest nie tak — patrz wskazówki wyżej.");
            std::process::exit(1);
        }
    }
    Ok(())
}

fn list_peers() -> Result<()> {
    let store = mb_net::KeyStore::open()?;
    let peers: Vec<&str> = store.peers().collect();
    if peers.is_empty() {
        println!("Nic jeszcze nie sparowane.");
        println!("Parowanie dzieje się samo przy pierwszym połączeniu.");
        return Ok(());
    }
    println!("SPAROWANE");
    for peer in peers {
        println!("  {peer}");
    }
    println!("\nKlucze: {}", store.path().display());
    println!("Zapomnienie: micbridge forget <nazwa> — po obu stronach.");
    Ok(())
}

fn forget_peer(peer: &str) -> Result<()> {
    let mut store = mb_net::KeyStore::open()?;
    if store.forget(peer)? {
        println!("Zapomniane: „{peer}”.");
        println!("Zrób to samo po drugiej stronie, inaczej nie dogadacie się bez kodu.");
    } else {
        println!("Nie mam nic zapisanego pod „{peer}”.");
    }
    Ok(())
}

fn list_devices() -> Result<()> {
    for dir in [Direction::Input, Direction::Output] {
        let heading = match dir {
            Direction::Input => "WEJŚCIA (źródła mikrofonu)",
            Direction::Output => "WYJŚCIA (ujścia — tu szukamy wirtualnego kabla)",
        };
        println!("\n{heading}");

        let devices = mb_audio::list(dir)?;
        if devices.is_empty() {
            println!("  (brak)");
            continue;
        }

        for d in &devices {
            let marker = if d.is_default { '*' } else { ' ' };
            let rate = d
                .default_sample_rate
                .map(|r| format!("{} Hz", r))
                .unwrap_or_else(|| "?".into());
            let ch = d
                .channels
                .map(|c| format!("{c} ch"))
                .unwrap_or_else(|| "?".into());
            let virtual_hint = if mb_audio::looks_like_virtual_cable(&d.name) {
                "  ← wirtualny kabel"
            } else {
                ""
            };

            println!(
                "{marker} @{:<2} {:<48} {:>9}  {:>6}{}",
                d.index, d.name, rate, ch, virtual_hint
            );
        }
    }

    println!(
        "\n* = domyślne w systemie.  Silnik pracuje przy {SAMPLE_RATE} Hz — \
         inne częstotliwości są przeliczane."
    );
    println!("Wskazywanie: --device \"yeti\" albo --device @3");
    if cfg!(target_os = "linux") {
        println!(
            "Ujście: --sink auto tworzy własny mikrofon „{}” w PipeWire.",
            mb_audio::DISPLAY_NAME
        );
        println!("        Nie ma go na liście powyżej — powstaje dopiero po połączeniu.");
    } else {
        println!("Ujście: --sink auto szuka wirtualnego kabla wśród wyjść;");
        println!("        Windows nie pozwala programowi utworzyć własnego mikrofonu.");
    }
    println!(
        "Bez mikrofonu: --device {} nadaje sinus 440 Hz — sprawdza całą ścieżkę.\n",
        send::TONE_SELECTOR
    );
    Ok(())
}
