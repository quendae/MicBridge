//! Strona odbierająca: bufor jitter, odtworzenie strat i wpuszczenie dźwięku
//! w wirtualne wejście.
//!
//! Trzy wątki i jeden callback audio:
//!   sieć     — odbiera pakiety, wkłada je do bufora jitter
//!   pacer    — wyjmuje, dekoduje, resampluje i dolewa do pierścienia
//!   callback — tylko opróżnia pierścień, bez blokad i alokacji
//!
//! Pacer jest miejscem, w którym zamyka się pętla regulacji dryfu: mierzy
//! głębokość bufora, pyta regulator o korektę i podaje ją resamplerowi.

use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, Context, Result};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::HeapRb;

use mb_engine::codec::frame_buffer;
use mb_engine::{
    AdaptiveTarget, DriftController, JitterBuffer, OpusDecoder, Pop, StreamStats, VariableResampler,
};
use mb_proto::{
    Accept, ControlMsg, PayloadKind, RtpHeader, SeqExtender, Stats, FRAME_MS, FRAME_SAMPLES,
    MEDIA_PORT, PROTOCOL_VERSION, RTP_HEADER_LEN, SAMPLE_RATE,
};

const RING_SAMPLES: usize = SAMPLE_RATE as usize;
/// Dolna granica zapasu przed callbackiem — dwie ramki.
///
/// Górna bierze się z tego, o ile karta faktycznie prosi. Stały zapas dwóch
/// ramek wystarczał przy WASAPI, ale PipeWire potrafi wołać o sto milisekund
/// naraz: wtedy każde wywołanie dostawało 20 ms dźwięku i 80 ms ciszy, a bufor
/// jitter puchł do sufitu, bo pacer dolewał tylko do własnego progu.
const RING_MIN: usize = FRAME_SAMPLES * 2;
/// Ile razy większy zapas trzymać niż największa zaobserwowana porcja.
const RING_HEADROOM: usize = 2;
/// Górna granica bufora jitter — powyżej wolimy uciąć niż rosnąć.
const MAX_BUFFER_MS: u32 = 400;
/// Dolna granica poduszki: jedna ramka nie przetrwa żadnego przestawienia.
const MIN_BUFFER_FRAMES: usize = 2;

pub fn run(listen: &str, sink: &str, buffer_ms: u32, adaptive: bool) -> Result<()> {
    let listen_addr: SocketAddr = listen
        .parse()
        .with_context(|| format!("`{listen}` nie jest adresem nasłuchu"))?;

    mb_audio::sink::validate(sink)?;

    let target_frames = (buffer_ms / FRAME_MS).max(1) as usize;
    let max_frames = (MAX_BUFFER_MS / FRAME_MS) as usize;

    let listener =
        TcpListener::bind(listen_addr).with_context(|| format!("nie mogę zająć {listen_addr}"))?;
    println!("Nasłuchuję na {listen_addr}. Ctrl-C kończy.");
    println!(
        "Bufor jitter: {buffer_ms} ms ({target_frames} × {FRAME_MS} ms){}.",
        if adaptive {
            ", adaptacyjny"
        } else {
            ", stały"
        }
    );

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
                let cfg = SessionConfig {
                    sink,
                    target_frames,
                    max_frames,
                    adaptive,
                };
                if let Err(e) = session(control, &cfg, &running) {
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

struct SessionConfig<'a> {
    sink: &'a str,
    target_frames: usize,
    max_frames: usize,
    adaptive: bool,
}

fn session(mut control: TcpStream, cfg: &SessionConfig, running: &Arc<AtomicBool>) -> Result<()> {
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
    // Największa porcja, o jaką poprosiło ujście. Pacer trzyma pierścień
    // powyżej tej wartości, bo inaczej nie ma szans jej pokryć.
    let request = Arc::new(AtomicU64::new(RING_MIN as u64));
    // Karta dźwiękowa rusza z opóźnieniem rzędu sekundy. Gdyby pacer zaczął
    // wtedy opróżniać bufor, przycięcie poduszki wypadłoby przed startem
    // urządzenia i cały nadmiar zebrany w międzyczasie zostałby na stałe —
    // regulator dryfu ściągałby go potem kilkanaście sekund.
    let playback_started = Arc::new(AtomicBool::new(false));

    let sink = {
        let starved = Arc::clone(&starved);
        let playback_started = Arc::clone(&playback_started);
        let request = Arc::clone(&request);
        mb_audio::open_sink(cfg.sink, SAMPLE_RATE, move |out| {
            playback_started.store(true, Ordering::Relaxed);
            request.fetch_max(out.len() as u64, Ordering::Relaxed);
            let got = consumer.pop_slice(out);
            if got < out.len() {
                out[got..].fill(0.0);
                starved.fetch_add((out.len() - got) as u64, Ordering::Relaxed);
            }
        })?
    };
    let device_rate = sink.sample_rate();
    tracing::info!(
        sink = %sink.name(),
        rate = device_rate,
        wejscie = sink.is_input_device(),
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
        sink: sink.name().to_string(),
        host: hostname(),
    })
    .write_to(&mut control)?;

    println!("\n{} ({}) → {}", hello.host, peer.ip(), sink.name());
    println!("  źródło: {}", hello.device);
    if device_rate != SAMPLE_RATE {
        println!("  konwersja {SAMPLE_RATE} Hz → {device_rate} Hz");
    }
    if sink.is_input_device() {
        println!(
            "  w aplikacji (Discord, OBS, gra) wybierz mikrofon „{}”",
            mb_audio::DISPLAY_NAME
        );
    } else {
        println!("  w aplikacji wybierz mikrofon odpowiadający temu urządzeniu");
        if let Some(hint) = mb_audio::latency_hint(sink.name()) {
            println!("  {hint}");
        }
    }

    let jitter = Arc::new(Mutex::new(JitterBuffer::new(
        cfg.target_frames,
        cfg.max_frames,
    )));
    let live = Arc::new(AtomicBool::new(true));
    let ring_samples = Arc::new(AtomicU64::new(0));
    let shared = Arc::new(SharedStats::default());

    // --- wątek sieciowy -----------------------------------------------------
    let net = {
        let jitter = Arc::clone(&jitter);
        let live = Arc::clone(&live);
        let running = Arc::clone(running);
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 2048];
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

                // Pierwszy pakiet ustala nadawcę; przypadkowy ruch na tym
                // porcie nie ma się mieszać w strumień.
                match expected_ssrc {
                    None => expected_ssrc = Some(header.ssrc),
                    Some(s) if s != header.ssrc => continue,
                    _ => {}
                }
                if header.payload != PayloadKind::Opus {
                    continue;
                }

                let ext = extender.extend(header.seq);
                stats.on_packet(ext, Instant::now());
                shared.set_jitter(stats.jitter_ms());

                if let Ok(mut jb) = jitter.lock() {
                    jb.push(ext, buf[RTP_HEADER_LEN..n].to_vec());
                }
            }
        })
    };

    // --- pacer: dekodowanie, odtwarzanie strat, regulacja dryfu -------------
    let pacer = {
        let jitter = Arc::clone(&jitter);
        let live = Arc::clone(&live);
        let running = Arc::clone(running);
        let ring_samples = Arc::clone(&ring_samples);
        let shared = Arc::clone(&shared);
        let playback_started = Arc::clone(&playback_started);
        let request = Arc::clone(&request);
        let adaptive = cfg.adaptive;
        let start_frames = cfg.target_frames;
        let max_frames = cfg.max_frames;

        std::thread::spawn(move || {
            if let Err(e) = pace(
                &jitter,
                &mut producer,
                &live,
                &running,
                &ring_samples,
                &shared,
                &playback_started,
                &request,
                device_rate,
                adaptive,
                start_frames,
                max_frames,
            ) {
                tracing::error!(error = %e, "pacer padł");
                live.store(false, Ordering::Relaxed);
            }
        })
    };

    // --- raportowanie -------------------------------------------------------
    let mut last = Instant::now();
    let mut seen_overflow = 0u64;
    while running.load(Ordering::Relaxed) && live.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(100));
        if last.elapsed() < Duration::from_secs(1) {
            continue;
        }
        last = Instant::now();

        let (depth, loss, late, recovered, stalls, overflow) = {
            let Ok(jb) = jitter.lock() else { break };
            (
                jb.depth(),
                jb.loss_pct(),
                jb.late,
                jb.recovered,
                jb.stalls,
                jb.dropped_overflow,
            )
        };
        let ring_ms = ring_samples.load(Ordering::Relaxed) as f32 * 1000.0 / device_rate as f32;
        let jitter_buf_ms = depth as f32 * FRAME_MS as f32;
        let starved_now = starved.swap(0, Ordering::Relaxed);

        println!(
            "  bufor {:>3.0}+{:>2.0} ms   cel {:>3.0}   strat {loss:>4.1}% (FEC {recovered})   \
             jitter {:>4.1} ms   dryf {:+.3}%{}",
            jitter_buf_ms,
            ring_ms,
            shared.setpoint(),
            shared.jitter(),
            shared.correction() * 100.0,
            // Przepełnienie bufora nie liczy się jako strata pakietu, ale
            // wyrzucone ramki słychać tak samo — bez tego licznika stan
            // „strat 0,0%” towarzyszył rwącemu się dźwiękowi.
            match (starved_now, overflow.saturating_sub(seen_overflow)) {
                (0, 0) => String::new(),
                (s, 0) => format!("   NIEDOMIAR {s}"),
                (0, o) => format!("   WYRZUCONO {o} ramek (bufor pełny)"),
                (s, o) => format!("   NIEDOMIAR {s}   WYRZUCONO {o}"),
            }
        );
        seen_overflow = overflow;

        let report = ControlMsg::Stats(Stats {
            lost_pct: loss,
            jitter_ms: shared.jitter(),
            buffer_ms: jitter_buf_ms + ring_ms,
            late_pct: late as f32,
        });
        if report.write_to(&mut control).is_err() {
            tracing::info!("nadajnik się rozłączył");
            break;
        }
        if stalls > 0 && starved_now > 0 {
            tracing::warn!(stalls, "strumień się zatrzymywał");
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

/// Wyjmuje ramki z bufora, dekoduje je (odtwarzając straty) i dolewa do
/// pierścienia, po drodze korygując dryf zegarów.
#[allow(clippy::too_many_arguments)]
fn pace<P: Producer<Item = f32> + Observer>(
    jitter: &Mutex<JitterBuffer>,
    producer: &mut P,
    live: &AtomicBool,
    running: &AtomicBool,
    ring_samples: &AtomicU64,
    shared: &SharedStats,
    playback_started: &AtomicBool,
    request: &AtomicU64,
    device_rate: u32,
    adaptive: bool,
    start_frames: usize,
    max_frames: usize,
) -> Result<()> {
    let mut decoder = OpusDecoder::new()?;
    let mut pcm = frame_buffer();
    let mut float = vec![0f32; FRAME_SAMPLES];
    let mut resampled: Vec<f32> = Vec::with_capacity(FRAME_SAMPLES * 2);

    // Jeden resampler robi obie rzeczy naraz: przelicza 48 kHz na
    // częstotliwość urządzenia i nakłada korektę dryfu.
    let mut resampler =
        VariableResampler::new(device_rate as f64 / SAMPLE_RATE as f64, FRAME_SAMPLES)?;

    let mut target = AdaptiveTarget::new(start_frames, MIN_BUFFER_FRAMES, max_frames);
    let mut drift = DriftController::new(start_frames as f32 * FRAME_MS as f32);
    shared.set_setpoint(drift.setpoint_ms());

    let mut last_tick = Instant::now();
    // Poduszkę przycinamy drugi raz, gdy pierścień jest już pełny — patrz
    // `JitterBuffer::trim_to_target`.
    let mut primed = false;
    // Poduszkę podnoszą wyłącznie pakiety spóźnione i puste przebiegi — one
    // znaczą, że czekaliśmy za krótko. Zwykła strata nic o tym nie mówi.
    let mut seen_late = 0u64;
    let mut seen_stalls = 0u64;

    while live.load(Ordering::Relaxed) && running.load(Ordering::Relaxed) {
        // Dopóki urządzenie nie zażądało pierwszej próbki, nie ruszamy bufora:
        // wtedy przycięcie poduszki wypadnie dokładnie na starcie odtwarzania,
        // a nie przed nim. Pierwszy callback dostanie ciszę — jednej ramki
        // nikt nie usłyszy, kilkunastu sekund nadmiarowego opóźnienia owszem.
        if !playback_started.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }

        // Zapas skrojony pod rzeczywistą porcję, o jaką woła ujście. Połowa
        // pierścienia to twardy sufit, żeby przy absurdalnym kwancie nie
        // próbować trzymać więcej, niż się mieści.
        let ring_target = (request.load(Ordering::Relaxed) as usize * RING_HEADROOM)
            .clamp(RING_MIN, RING_SAMPLES / 2);

        while producer.occupied_len() < ring_target && producer.vacant_len() >= FRAME_SAMPLES * 2 {
            let popped = {
                let Ok(mut jb) = jitter.lock() else { break };
                jb.pop()
            };

            let decoded = match popped {
                Pop::Packet(p) => decoder.decode(&p, &mut pcm)?,
                // Ramka zginęła, ale następna niesie jej zapasową kopię.
                // Poduszki nie ruszamy: zgubiony pakiet nie przyjdzie nigdy,
                // choćbyśmy czekali dowolnie długo.
                Pop::LostRecoverable(next) => decoder.decode_fec(&next, &mut pcm)?,
                // Nie ma z czego odtwarzać — dekoder dopowiada z własnego modelu.
                Pop::Lost => decoder.conceal(&mut pcm)?,
                Pop::Filling => break,
            };

            for (dst, &src) in float.iter_mut().zip(pcm[..decoded].iter()) {
                *dst = src as f32 / 32768.0;
            }
            if decoded < FRAME_SAMPLES {
                float[decoded..].fill(0.0);
            }

            resampled.clear();
            resampler.process(&float, &mut resampled)?;
            producer.push_slice(&resampled);
        }

        let occupied = producer.occupied_len();
        ring_samples.store(occupied as u64, Ordering::Relaxed);

        if !primed && occupied >= ring_target {
            primed = true;
            if let Ok(mut jb) = jitter.lock() {
                let dropped = jb.trim_to_target();
                if dropped > 0 {
                    tracing::debug!(dropped, "przycięto poduszkę po napełnieniu potoku");
                }
            }
            drift.reset();
        }

        // Regulacja co 20 ms; szybciej nie ma sensu przy stałej czasowej 2 s.
        let dt = last_tick.elapsed();
        if dt >= Duration::from_millis(20) {
            last_tick = Instant::now();

            let (depth, playing, late, stalls) = {
                let Ok(jb) = jitter.lock() else { break };
                (jb.depth(), jb.playing(), jb.late, jb.stalls)
            };

            if adaptive && (late > seen_late || stalls > seen_stalls) {
                target.on_late(Instant::now());
            }
            seen_late = late;
            seen_stalls = stalls;

            if playing {
                let correction = drift.update(depth as f32 * FRAME_MS as f32, dt.as_secs_f32());
                resampler.set_correction(correction)?;
                shared.set_correction(correction);
            } else {
                // Po zatrzymaniu strumienia całka opisuje świat, którego już nie ma.
                drift.reset();
                resampler.set_correction(0.0)?;
                shared.set_correction(0.0);
            }

            if adaptive {
                target.tick(Instant::now());
                let wanted = target.frames() as f32 * FRAME_MS as f32;
                if wanted != drift.setpoint_ms() {
                    if let Ok(mut jb) = jitter.lock() {
                        jb.set_target_frames(target.frames());
                    }
                    drift.set_setpoint(wanted);
                    shared.set_setpoint(wanted);
                    tracing::debug!(ms = wanted, "nowy cel poduszki");
                }
            }
        }

        std::thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}

/// Liczniki dzielone między wątkami. f32 trzymane w bitach, bo `AtomicF32`
/// nie istnieje w bibliotece standardowej.
#[derive(Default)]
struct SharedStats {
    jitter_ms: AtomicU64,
    correction: AtomicU64,
    setpoint_ms: AtomicU64,
}

impl SharedStats {
    fn set_jitter(&self, v: f32) {
        self.jitter_ms.store(v.to_bits() as u64, Ordering::Relaxed);
    }
    fn jitter(&self) -> f32 {
        f32::from_bits(self.jitter_ms.load(Ordering::Relaxed) as u32)
    }
    fn set_correction(&self, v: f64) {
        self.correction.store(v.to_bits(), Ordering::Relaxed);
    }
    fn correction(&self) -> f64 {
        f64::from_bits(self.correction.load(Ordering::Relaxed))
    }
    fn set_setpoint(&self, v: f32) {
        self.setpoint_ms
            .store(v.to_bits() as u64, Ordering::Relaxed);
    }
    fn setpoint(&self) -> f32 {
        f32::from_bits(self.setpoint_ms.load(Ordering::Relaxed) as u32)
    }
}

fn check(hello: &mb_proto::Hello) -> std::result::Result<(), String> {
    if hello.version != PROTOCOL_VERSION {
        return Err(format!(
            "wersja protokołu {} (obsługuję {PROTOCOL_VERSION})",
            hello.version
        ));
    }
    if hello.sample_rate != SAMPLE_RATE {
        return Err(format!(
            "częstotliwość {} Hz (obsługuję {SAMPLE_RATE})",
            hello.sample_rate
        ));
    }
    if hello.channels != 1 {
        return Err(format!("{} kanałów (obsługuję mono)", hello.channels));
    }
    if hello.frame_ms != FRAME_MS {
        return Err(format!(
            "ramka {} ms (obsługuję {FRAME_MS})",
            hello.frame_ms
        ));
    }
    if hello.payload != PayloadKind::Opus {
        return Err("obsługuję wyłącznie Opusa".into());
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
