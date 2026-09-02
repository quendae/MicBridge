//! Strona odbierająca: bufor jitter i wpuszczenie dźwięku w wirtualne wejście.
//!
//! Trzy wątki i jeden callback audio:
//!   sieć    — odbiera pakiety, wkłada ramki do bufora jitter
//!   pacer   — wyjmuje ramki z bufora i dolewa je do pierścienia
//!   callback— tylko opróżnia pierścień, bez blokad i alokacji
//!
//! Rozdzielenie pacera od callbacku jest miejscem, w które w M2 wejdzie
//! regulator dryfu: to on będzie decydował, jak szybko dolewać.

use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, Context, Result};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::HeapRb;

use mb_engine::{JitterBuffer, Pop, StreamStats};
use mb_proto::{
    rtp::decode_pcm, Accept, ControlMsg, PayloadKind, RtpHeader, SeqExtender, Stats, FRAME_MS,
    FRAME_SAMPLES, MEDIA_PORT, PROTOCOL_VERSION, RTP_HEADER_LEN, SAMPLE_RATE,
};

const RING_SAMPLES: usize = SAMPLE_RATE as usize;
/// Ile dźwięku trzymać przed callbackiem. Dwie ramki wystarczą, żeby nie
/// zagłodzić karty, a nie dokładają zauważalnego opóźnienia.
const RING_TARGET: usize = FRAME_SAMPLES * 2;
/// Górna granica bufora jitter — powyżej niej wolimy uciąć niż rosnąć.
const MAX_BUFFER_MS: u32 = 400;

pub fn run(listen: &str, sink: &str, buffer_ms: u32) -> Result<()> {
    let listen_addr: SocketAddr = listen
        .parse()
        .with_context(|| format!("`{listen}` nie jest adresem nasłuchu"))?;

    let target_frames = (buffer_ms / FRAME_MS).max(1) as usize;
    let max_frames = (MAX_BUFFER_MS / FRAME_MS) as usize;

    let listener = TcpListener::bind(listen_addr)
        .with_context(|| format!("nie mogę zająć {listen_addr}"))?;
    println!("Nasłuchuję na {listen_addr}. Ctrl-C kończy.");
    println!("Bufor jitter: {buffer_ms} ms ({target_frames} ramek po {FRAME_MS} ms).");

    let running = Arc::new(AtomicBool::new(true));
    {
        let running = Arc::clone(&running);
        ctrlc::set_handler(move || running.store(false, Ordering::Relaxed))
            .context("nie mogę przechwycić Ctrl-C")?;
    }

    for stream in listener.incoming() {
        if !running.load(Ordering::Relaxed) {
            break;
        }
        match stream {
            Ok(control) => {
                if let Err(e) = session(control, sink, target_frames, max_frames, &running) {
                    tracing::error!(error = %e, "sesja zakończona błędem");
                }
                if !running.load(Ordering::Relaxed) {
                    break;
                }
                println!("\nCzekam na kolejne połączenie.");
            }
            Err(e) => tracing::warn!(error = %e, "nieudane połączenie przychodzące"),
        }
    }
    Ok(())
}

fn session(
    mut control: TcpStream,
    sink: &str,
    target_frames: usize,
    max_frames: usize,
    running: &Arc<AtomicBool>,
) -> Result<()> {
    let peer = control.peer_addr()?;
    control.set_nodelay(true)?;
    tracing::info!(%peer, "połączenie przychodzące");

    let hello = match ControlMsg::read_from(&mut control)? {
        ControlMsg::Hello(h) => h,
        other => bail!("oczekiwałem HELLO, dostałem {other:?}"),
    };

    if let Err(reason) = check(&hello) {
        tracing::warn!(%reason, "odrzucam");
        let _ = ControlMsg::Reject {
            reason: reason.clone(),
        }
        .write_to(&mut control);
        bail!(reason);
    }

    // Ujście otwieramy dopiero po HELLO, żeby komunikat o braku wirtualnego
    // kabla pojawił się w kontekście konkretnej próby połączenia.
    let rb = HeapRb::<f32>::new(RING_SAMPLES);
    let (mut producer, mut consumer) = rb.split();
    let starved = Arc::new(AtomicU64::new(0));

    let playback = {
        let starved = Arc::clone(&starved);
        mb_audio::start_playback(sink, SAMPLE_RATE, move |out| {
            let got = consumer.pop_slice(out);
            if got < out.len() {
                out[got..].fill(0.0);
                starved.fetch_add((out.len() - got) as u64, Ordering::Relaxed);
            }
        })?
    };
    tracing::info!(
        device = %playback.device_name,
        channels = playback.channels,
        "ujście otwarte"
    );

    let media = UdpSocket::bind(SocketAddr::new(control.local_addr()?.ip(), MEDIA_PORT))
        .or_else(|_| UdpSocket::bind(("0.0.0.0", MEDIA_PORT)))
        .with_context(|| format!("nie mogę zająć portu UDP {MEDIA_PORT}"))?;
    media.set_read_timeout(Some(Duration::from_millis(200)))?;
    let ssrc = fresh_ssrc();

    ControlMsg::Accept(Accept {
        version: PROTOCOL_VERSION,
        ssrc,
        media_port: MEDIA_PORT,
        sink: playback.device_name.clone(),
        host: hostname(),
    })
    .write_to(&mut control)?;

    println!(
        "\n{} ({}) → {}\n  źródło: {}",
        hello.host, peer.ip(), playback.device_name, hello.device
    );

    let jitter = Arc::new(Mutex::new(JitterBuffer::new(target_frames, max_frames)));
    let live = Arc::new(AtomicBool::new(true));
    // Poduszka to nie tylko bufor jitter: pierścień przed kartą też trzyma
    // dźwięk. Raportowanie samego bufora zaniżałoby opóźnienie o dwie ramki.
    let ring_samples = Arc::new(AtomicU64::new(0));

    // --- wątek sieciowy -----------------------------------------------------
    let net = {
        let jitter = Arc::clone(&jitter);
        let live = Arc::clone(&live);
        let running = Arc::clone(running);
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 2048];
            let mut samples = Vec::with_capacity(FRAME_SAMPLES);
            let mut extender = SeqExtender::new();
            let mut stats = StreamStats::new(FRAME_MS);
            let mut expected_ssrc = None;

            while live.load(Ordering::Relaxed) && running.load(Ordering::Relaxed) {
                let n = match media.recv(&mut buf) {
                    Ok(n) => n,
                    Err(ref e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "odbiór UDP");
                        continue;
                    }
                };

                let header = match RtpHeader::decode(&buf[..n]) {
                    Ok(h) => h,
                    Err(e) => {
                        tracing::debug!(error = %e, "odrzucony pakiet");
                        continue;
                    }
                };

                // Pierwszy pakiet ustala nadawcę; resztę ignorujemy, żeby
                // przypadkowy ruch na tym porcie nie mieszał się w strumień.
                match expected_ssrc {
                    None => expected_ssrc = Some(header.ssrc),
                    Some(s) if s != header.ssrc => continue,
                    _ => {}
                }

                if header.payload != PayloadKind::PcmS16 {
                    continue;
                }

                decode_pcm(&buf[RTP_HEADER_LEN..n], &mut samples);
                let ext = extender.extend(header.seq);
                stats.on_packet(ext, Instant::now());

                if let Ok(mut jb) = jitter.lock() {
                    jb.push(ext, std::mem::take(&mut samples));
                }
                samples = Vec::with_capacity(FRAME_SAMPLES);

                JITTER_MS.store(stats.jitter_ms().to_bits() as u64, Ordering::Relaxed);
            }
        })
    };

    // --- pacer --------------------------------------------------------------
    let pacer = {
        let jitter = Arc::clone(&jitter);
        let live = Arc::clone(&live);
        let running = Arc::clone(running);
        let ring_samples = Arc::clone(&ring_samples);
        std::thread::spawn(move || {
            let mut scratch = vec![0f32; FRAME_SAMPLES];
            while live.load(Ordering::Relaxed) && running.load(Ordering::Relaxed) {
                while producer.occupied_len() < RING_TARGET
                    && producer.vacant_len() >= FRAME_SAMPLES
                {
                    let popped = {
                        let Ok(mut jb) = jitter.lock() else { break };
                        jb.pop()
                    };
                    match popped {
                        Pop::Frame(frame) => {
                            for (dst, &src) in scratch.iter_mut().zip(frame.iter()) {
                                *dst = src as f32 / 32768.0;
                            }
                            // Ramka krótsza niż nominalna: dopełnij ciszą.
                            if frame.len() < FRAME_SAMPLES {
                                scratch[frame.len()..].fill(0.0);
                            }
                            producer.push_slice(&scratch);
                        }
                        // M2: tu wejdzie PLC Opusa zamiast ciszy.
                        Pop::Lost => {
                            scratch.fill(0.0);
                            producer.push_slice(&scratch);
                        }
                        Pop::Filling => break,
                    }
                }
                ring_samples.store(producer.occupied_len() as u64, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(2));
            }
        })
    };

    // --- raportowanie -------------------------------------------------------
    let mut last = Instant::now();
    while running.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(100));
        if last.elapsed() < Duration::from_secs(1) {
            continue;
        }
        last = Instant::now();

        let (depth, loss, late) = {
            let Ok(jb) = jitter.lock() else { break };
            (jb.depth(), jb.loss_pct(), jb.late)
        };
        let jitter_ms = f32::from_bits(JITTER_MS.load(Ordering::Relaxed) as u32);
        let ring_ms =
            ring_samples.load(Ordering::Relaxed) as f32 * 1000.0 / SAMPLE_RATE as f32;
        let buffer_ms = depth as f32 * FRAME_MS as f32 + ring_ms;
        let starved_now = starved.swap(0, Ordering::Relaxed);

        println!(
            "  bufor {buffer_ms:>5.0} ms   strat {loss:>4.1}%   jitter {jitter_ms:>4.1} ms\
             {}",
            if starved_now > 0 {
                format!("   NIEDOMIAR {starved_now} próbek")
            } else {
                String::new()
            }
        );

        let report = ControlMsg::Stats(Stats {
            lost_pct: loss,
            jitter_ms,
            buffer_ms,
            late_pct: late as f32,
        });
        if report.write_to(&mut control).is_err() {
            tracing::info!("nadajnik się rozłączył");
            break;
        }
    }

    live.store(false, Ordering::Relaxed);
    let _ = ControlMsg::Bye {
        reason: "koniec sesji".into(),
    }
    .write_to(&mut control);
    let _ = net.join();
    let _ = pacer.join();
    Ok(())
}

/// Jedyna wartość, którą wątek sieciowy musi pokazać wątkowi raportującemu.
/// f32 przechowywany w bitach, bo AtomicF32 nie istnieje.
static JITTER_MS: AtomicU64 = AtomicU64::new(0);

fn check(hello: &mb_proto::Hello) -> std::result::Result<(), String> {
    if hello.version != PROTOCOL_VERSION {
        return Err(format!(
            "wersja protokołu {} (obsługuję {PROTOCOL_VERSION})",
            hello.version
        ));
    }
    if hello.sample_rate != SAMPLE_RATE {
        return Err(format!("częstotliwość {} Hz (obsługuję {SAMPLE_RATE})", hello.sample_rate));
    }
    if hello.channels != 1 {
        return Err(format!("{} kanałów (obsługuję mono)", hello.channels));
    }
    if hello.frame_ms != FRAME_MS {
        return Err(format!("ramka {} ms (obsługuję {FRAME_MS})", hello.frame_ms));
    }
    if hello.payload != PayloadKind::PcmS16 {
        return Err("M1 obsługuje wyłącznie surowy PCM".into());
    }
    Ok(())
}

fn fresh_ssrc() -> u32 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() ^ (d.as_secs() as u32))
        .unwrap_or(0x7A31_F0C2)
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "nieznany".into())
}
