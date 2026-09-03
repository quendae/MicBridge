//! MicBridge — przenosi mikrofon z jednego komputera na drugi.
//!
//! Etap M2: Opus z FEC, bufor adaptacyjny i korekcja dryfu zegarów.
//! Adres nadal podaje się ręcznie — wykrywanie mDNS, parowanie i okno
//! dochodzą w M4.

mod recv;
mod send;

use anyhow::Result;
use clap::{Parser, Subcommand};
use mb_audio::Direction;
use mb_proto::{CONTROL_PORT, SAMPLE_RATE};

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

    /// Wysyłaj mikrofon do drugiego komputera.
    Send {
        /// Adres odbiornika: `192.168.1.40` albo `192.168.1.40:47100`.
        #[arg(long, value_name = "ADRES")]
        to: String,

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
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    match cli.command {
        Command::Devices => list_devices(),
        Command::Send {
            to,
            device,
            gain_db,
            bitrate,
            drop_pct,
        } => send::run(&to, &device, gain_db, bitrate, drop_pct),
        Command::Recv {
            listen,
            sink,
            buffer_ms,
            fixed_buffer,
        } => recv::run(&listen, &sink, buffer_ms, !fixed_buffer),
    }
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
