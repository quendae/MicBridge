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
use mb_i18n::{t, t1, t2, Key as K};
use mb_proto::{CONTROL_PORT, PROTOCOL_VERSION, SAMPLE_RATE};

#[derive(Parser)]
#[command(name = "micbridge", version, about = t(K::CliAbout), long_about = None)]
struct Cli {
    /// Więcej logów (można powtórzyć: -vv).
    #[arg(short, long, action = clap::ArgAction::Count, global = true, help = t(K::HelpVerbose))]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Wypisz urządzenia audio widoczne w systemie.
    #[command(about = t(K::HelpDevices))]
    Devices,

    /// Sprawdź, czy ta maszyna jest gotowa wysyłać i odbierać.
    #[command(about = t(K::HelpDoctor))]
    Doctor,

    /// Wypisz sparowane maszyny.
    #[command(about = t(K::HelpPeers))]
    Peers,

    /// Zapomnij parowanie z podaną maszyną.
    #[command(about = t(K::HelpForget))]
    Forget {
        /// Nazwa z listy `peers`.
        #[arg(value_name = t(K::VnName), help = t(K::HelpForgetPeer))]
        peer: String,
    },

    /// Pokaż odbiorniki widoczne w sieci lokalnej.
    #[command(about = t(K::HelpDiscover))]
    Discover {
        /// Jak długo słuchać odpowiedzi.
        #[arg(long, default_value_t = 2500, value_name = "MS", help = t(K::HelpWindowMs))]
        window_ms: u64,
    },

    /// Wysyłaj mikrofon do drugiego komputera.
    #[command(about = t(K::HelpSend))]
    Send {
        /// Odbiornik: adres, `adres:port` albo nazwa z `micbridge discover`.
        /// Bez tej flagi szukamy go w sieci.
        #[arg(long, value_name = t(K::VnAddress), help = t(K::HelpTo))]
        to: Option<String>,

        /// Źródło: `default`, `@3` albo fragment nazwy, np. `yeti`.
        #[arg(long, default_value = "default", value_name = t(K::VnDevice), help = t(K::HelpDevice))]
        device: String,

        /// Wzmocnienie w decybelach, dodatnie podbija cichy mikrofon.
        #[arg(long, default_value_t = 0.0, value_name = "dB", help = t(K::HelpGain))]
        gain_db: f32,

        /// Przepływność Opusa. 24 kbps jest przezroczyste dla mowy.
        #[arg(long, default_value_t = 24_000, value_name = "bps", help = t(K::HelpBitrate))]
        bitrate: u32,

        /// Diagnostyka: gub celowo tyle procent pakietów, żeby sprawdzić FEC.
        #[arg(long, default_value_t = 0.0, value_name = "PCT", help = t(K::HelpDropPct))]
        drop_pct: f32,

        /// Kod parowania, gdy nie ma gdzie go wpisać (skrypty, usługi).
        #[arg(long, value_name = t(K::VnDigits), help = t(K::HelpCode))]
        code: Option<String>,
    },

    /// Odbieraj mikrofon i wpuszczaj go w wirtualne urządzenie.
    #[command(about = t(K::HelpRecv))]
    Recv {
        /// Interfejs i port do nasłuchu.
        #[arg(long, default_value_t = format!("0.0.0.0:{CONTROL_PORT}"), value_name = t(K::VnAddress), help = t(K::HelpListen))]
        listen: String,

        /// Ujście: `auto` (Linux: własny mikrofon, Windows: wirtualny kabel),
        /// `virtual`, `device` albo fragment nazwy urządzenia.
        #[arg(long, default_value = "auto", value_name = t(K::VnSink), help = t(K::HelpSink))]
        sink: String,

        /// Startowa głębokość bufora jitter.
        #[arg(long, default_value_t = 30, value_name = "MS", help = t(K::HelpBufferMs))]
        buffer_ms: u32,

        /// Nie dopasowuj poduszki do jakości łącza — trzymaj zadaną wartość.
        #[arg(long, help = t(K::HelpFixedBuffer))]
        fixed_buffer: bool,

        /// Nie ogłaszaj się w sieci — druga strona poda adres ręcznie.
        #[arg(long, help = t(K::HelpNoAnnounce))]
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
    ctrlc::set_handler(move || flag.store(false, Ordering::Relaxed)).context(t(K::ErrCtrlC))?;
    println!("{}", t(K::CliCtrlC));
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
    println!("{}", t1(K::CliSearching, window_ms));
    let peers = mb_net::browse(std::time::Duration::from_millis(window_ms))?;

    if peers.is_empty() {
        println!("\n{}", t(K::CliNothingSeen));
        println!("{}", t(K::CliHintRecv));
        println!("{}", t(K::CliHintSameNet));
        println!("{}", t(K::CliHintMulticast));
        println!("{}", t(K::CliHintUseTo));
        return Ok(());
    }

    println!("\n{}", t(K::CliReceivers));
    for peer in &peers {
        let note = if peer.compatible() {
            String::new()
        } else {
            t2(K::CliProtocolInstead, peer.version, PROTOCOL_VERSION)
        };
        println!("  {:<24} {:<24}{note}", peer.name, peer.addr.to_string());
    }
    println!("\n{}", t1(K::CliSendWith, &peers[0].name));
    Ok(())
}

/// Przegląd gotowości. Kod wyjścia niezerowy przy błędzie, żeby dało się
/// tego użyć w skrypcie albo w instalatorze.
fn doctor() -> Result<()> {
    let report = mb_app::doctor::check();
    println!("{}\n", t(K::DocTitle));
    for check in &report.checks {
        println!("{check}");
    }

    println!();
    match report.worst() {
        mb_app::doctor::Grade::Ok => println!("{}", t(K::DocAllGood)),
        mb_app::doctor::Grade::Warn => {
            println!("{}", t(K::DocPartial));
        }
        mb_app::doctor::Grade::Fail => {
            println!("{}", t(K::DocBroken));
            std::process::exit(1);
        }
    }
    Ok(())
}

fn list_peers() -> Result<()> {
    let store = mb_net::KeyStore::open()?;
    let peers: Vec<&str> = store.peers().collect();
    if peers.is_empty() {
        println!("{}", t(K::CliNothingPaired));
        println!("{}", t(K::CliPairingIsAutomatic));
        return Ok(());
    }
    println!("{}", t(K::CliPairedHeading));
    for peer in peers {
        println!("  {peer}");
    }
    println!("\n{}", t1(K::CliKeysAt, store.path().display()));
    println!("{}", t(K::CliForgetHint));
    Ok(())
}

fn forget_peer(peer: &str) -> Result<()> {
    let mut store = mb_net::KeyStore::open()?;
    if store.forget(peer)? {
        println!("{}", t1(K::CliForgotten, peer));
        println!("{}", t(K::CliForgetOther));
    } else {
        println!("{}", t1(K::CliNothingUnder, peer));
    }
    Ok(())
}

fn list_devices() -> Result<()> {
    for dir in [Direction::Input, Direction::Output] {
        let heading = match dir {
            Direction::Input => t(K::CliInputs),
            Direction::Output => t(K::CliOutputs),
        };
        println!("\n{heading}");

        let devices = mb_audio::list(dir)?;
        if devices.is_empty() {
            println!("{}", t(K::CliNone));
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
                t(K::CliVirtualCable)
            } else {
                ""
            };

            println!(
                "{marker} @{:<2} {:<48} {:>9}  {:>6}{}",
                d.index, d.name, rate, ch, virtual_hint
            );
        }
    }

    println!("\n{}", t1(K::CliDefaultMark, SAMPLE_RATE));
    println!("{}", t(K::CliPointing));
    if cfg!(target_os = "linux") {
        println!("{}", t1(K::CliSinkLinux, mb_audio::DISPLAY_NAME));
        println!("{}", t(K::CliSinkLinuxNote));
    } else {
        println!("{}", t(K::CliSinkWindows));
        println!("{}", t(K::CliSinkWindowsNote));
    }
    println!("{}\n", t1(K::CliNoMic, send::TONE_SELECTOR));
    Ok(())
}
