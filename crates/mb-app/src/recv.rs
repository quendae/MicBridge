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
use mb_i18n::{t, t1, t2, Key as K};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::HeapRb;

use crate::ui::{RecvStatus, Reporter};
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
/// Ile zapasu założyć, zanim ujście pierwszy raz powie, o ile prosi.
///
/// Cały pierścień musi się nazbierać, zanim karta zacznie pobierać — potem
/// produkcja równa się konsumpcji i poziom już nie rośnie. Zakładamy więc
/// hojnie; nadmiar leci przy przycięciu po napełnieniu potoku i nie kosztuje
/// ani milisekundy opóźnienia.
const RING_ASSUMED_MS: usize = 100;
/// Górna granica bufora jitter — powyżej wolimy uciąć niż rosnąć.
const MAX_BUFFER_MS: u32 = 400;
/// Dolna granica poduszki: jedna ramka nie przetrwa żadnego przestawienia.
const MIN_BUFFER_FRAMES: usize = 2;
/// Po tylu milisekundach bez wywołania uznajemy, że ujścia nikt nie słucha.
///
/// W Linuksie węzeł istnieje od razu, ale PipeWire uruchamia nasz callback
/// dopiero, gdy ktoś się do niego podepnie. Bez tego progu pakiety piętrzyły
/// się do sufitu bufora, zanim użytkownik zdążył wybrać mikrofon w aplikacji.
const SINK_IDLE_MS: u64 = 250;

/// Jak ma pracować odbiornik.
#[derive(Debug, Clone)]
pub struct Options {
    pub listen: String,
    /// Ujście: `auto`, `virtual`, `device` albo fragment nazwy.
    pub sink: String,
    pub buffer_ms: u32,
    /// Czy dopasowywać poduszkę do jakości łącza.
    pub adaptive: bool,
    /// Czy ogłaszać się w sieci przez mDNS.
    pub announce: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            listen: format!("0.0.0.0:{}", mb_proto::CONTROL_PORT),
            sink: "auto".into(),
            buffer_ms: 30,
            adaptive: true,
            announce: true,
        }
    }
}

/// Ile czekamy między próbami przyjęcia połączenia.
///
/// Nasłuch jest nieblokujący, żeby prośba o zatrzymanie działała od razu.
/// Blokujące `accept` reagowałoby dopiero, gdy ktoś się połączy.
const ACCEPT_POLL: Duration = Duration::from_millis(100);

pub fn run(opts: &Options, ui: &dyn Reporter, running: Arc<AtomicBool>) -> Result<()> {
    let listen_addr: SocketAddr = opts
        .listen
        .parse()
        .with_context(|| format!("`{}` nie jest adresem nasłuchu", opts.listen))?;

    mb_audio::sink::validate(&opts.sink)?;

    let target_frames = (opts.buffer_ms / FRAME_MS).max(1) as usize;
    let max_frames = (MAX_BUFFER_MS / FRAME_MS) as usize;

    let listener =
        TcpListener::bind(listen_addr).with_context(|| t1(K::ErrCannotBind, listen_addr))?;
    listener.set_nonblocking(true)?;
    ui.line(&t1(K::SesListening, listen_addr));

    // Ogłoszenie żyje tak długo jak nasłuch. Nie zrywamy go na czas sesji:
    // druga maszyna ma widzieć ten komputer także wtedy, gdy akurat jest
    // zajęty — inaczej lista migałaby zależnie od tego, kto się właśnie łączy.
    let _beacon = if opts.announce {
        match mb_net::Advertiser::start(listen_addr.port()) {
            Ok(a) => {
                ui.line(&t1(K::SesVisibleAs, mb_net::hostname()));
                Some(a)
            }
            Err(e) => {
                // Brak multicastu nie może zatrzymać odbiornika: adres da się
                // wpisać ręcznie i to jest cała droga awaryjna.
                tracing::warn!(error = %e, "nie mogę ogłosić się w sieci");
                ui.line(t(K::SesCannotAnnounce));
                None
            }
        }
    } else {
        None
    };
    ui.line(&format!(
        "Bufor jitter: {} ms ({target_frames} × {FRAME_MS} ms){}.",
        opts.buffer_ms,
        if opts.adaptive {
            ", adaptacyjny"
        } else {
            ", stały"
        }
    ));

    // Kod parowania żyje tak długo jak proces, nie jak połączenie: użytkownik
    // przepisuje go z tego ekranu i ma mieć czas się pomylić.
    let pairing = Mutex::new(crate::pair::Pairing::new());

    while running.load(Ordering::Relaxed) {
        let stream = match listener.accept() {
            Ok((control, _)) => Ok(control),
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(ACCEPT_POLL);
                continue;
            }
            Err(e) => Err(e),
        };
        match stream {
            Ok(control) => {
                let cfg = SessionConfig {
                    sink: &opts.sink,
                    target_frames,
                    max_frames,
                    adaptive: opts.adaptive,
                };
                if let Err(e) = session(control, &cfg, &running, &pairing, ui) {
                    tracing::error!(error = %e, "sesja zakończona błędem");
                    ui.line(&t1(K::SesEnded, e));
                }
                if !running.load(Ordering::Relaxed) {
                    break;
                }
                ui.line(t(K::SesWaitNext));
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

fn session(
    mut control: TcpStream,
    cfg: &SessionConfig,
    running: &Arc<AtomicBool>,
    pairing: &Mutex<crate::pair::Pairing>,
    ui: &dyn Reporter,
) -> Result<()> {
    let peer = control.peer_addr()?;
    // Gniazdo dziedziczy tryb nieblokujący po nasłuchu, a sesja czyta wprost
    // ze strumienia i oczekuje, że odczyt poczeka na dane.
    control.set_nonblocking(false)?;
    control.set_nodelay(true)?;
    tracing::info!(%peer, "połączenie przychodzące");

    // Rozpoznanie i ewentualne parowanie idą jawnie; wszystko dalej już nie.
    let (channel, peer_name) = crate::pair::accept(&mut control, pairing, ui)?;
    let channel = Mutex::new(channel);
    tracing::info!(%peer_name, "kanał sterujący zaszyfrowany");

    let hello = match crate::pair::recv_secure(&mut control, &channel)? {
        ControlMsg::Hello(h) => h,
        other => bail!("oczekiwałem HELLO, dostałem {other:?}"),
    };

    if let Err(reason) = check(&hello) {
        tracing::warn!(%reason, "odrzucam");
        let _ = crate::pair::send_secure(
            &mut control,
            &channel,
            &ControlMsg::Reject {
                reason: reason.clone(),
            },
        );
        bail!(reason);
    }

    // Ujście otwieramy dopiero po HELLO, żeby komunikat o braku wirtualnego
    // kabla pojawił się w kontekście konkretnej próby połączenia.
    let rb = HeapRb::<f32>::new(RING_SAMPLES);
    let (mut producer, mut consumer) = rb.split();
    let starved = Arc::new(AtomicU64::new(0));
    // Licznik wywołań ujścia. Jego brak ruchu znaczy, że po drugiej stronie
    // nikt nie słucha — patrz `SINK_IDLE_MS`.
    let served = Arc::new(AtomicU64::new(0));
    // Największa porcja, o jaką poprosiło ujście. Pacer trzyma pierścień
    // powyżej tej wartości, bo inaczej nie ma szans jej pokryć.
    let request = Arc::new(AtomicU64::new(RING_MIN as u64));
    let sink = {
        let starved = Arc::clone(&starved);
        let served = Arc::clone(&served);
        let request = Arc::clone(&request);
        mb_audio::open_sink(cfg.sink, SAMPLE_RATE, move |out| {
            served.fetch_add(1, Ordering::Relaxed);
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

    // Klucz mediów jest jednorazowy i jedzie zaszyfrowanym kanałem, więc nie
    // ma tu żadnego stanu do uzgadniania — a nagrany ruch nie da się odtworzyć
    // nawet komuś, kto później zdobędzie sekret z parowania.
    let media_key = mb_net::fresh_media_key();
    let cipher = mb_net::MediaCipher::new(&media_key)?;

    crate::pair::send_secure(
        &mut control,
        &channel,
        &ControlMsg::Accept(Accept {
            version: PROTOCOL_VERSION,
            ssrc,
            media_port: MEDIA_PORT,
            sink: sink.name().to_string(),
            host: mb_net::hostname(),
            media_key: media_key.to_vec(),
        }),
    )?;

    ui.connected(
        &format!("{} ({})", peer_name, peer.ip()),
        &t2(K::SesSource, sink.name(), &hello.device),
    );
    if device_rate != SAMPLE_RATE {
        ui.line(&t2(K::SesConversion, SAMPLE_RATE, device_rate));
    }
    if sink.is_input_device() {
        ui.line(&t1(K::SesPickMicNamed, mb_audio::DISPLAY_NAME));
    } else {
        ui.line(t(K::SesPickMicDevice));
        if let Some(hint) = mb_audio::latency_hint(sink.name()) {
            ui.line(&format!("  {hint}"));
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
            let mut rejected = 0u64;

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

                // Numer sekwencyjny podglądamy, zamiast go od razu przyjąć:
                // podrobiony pakiet nie może przesunąć licznika zawinięć, bo
                // rozjechałby wartości jednorazowe prawdziwym pakietom.
                let ext = extender.peek(header.seq);
                let payload = match cipher.open(
                    ext,
                    &buf[..RTP_HEADER_LEN],
                    &buf[RTP_HEADER_LEN..n],
                ) {
                    Ok(p) => p,
                    Err(_) => {
                        rejected += 1;
                        if rejected == 1 {
                            tracing::warn!(
                                    "pakiet nie przeszedł uwierzytelnienia — ktoś nadaje                                      na ten port albo druga strona ma inny klucz"
                                );
                        }
                        continue;
                    }
                };
                extender.extend(header.seq);

                stats.on_packet(ext, Instant::now());
                shared.set_jitter(stats.jitter_ms());

                if let Ok(mut jb) = jitter.lock() {
                    jb.push(ext, payload);
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
        let served = Arc::clone(&served);
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
                &served,
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

        ui.recv_status(&RecvStatus {
            buffer_ms: jitter_buf_ms,
            ring_ms,
            target_ms: shared.setpoint(),
            loss_pct: loss,
            recovered,
            jitter_ms: shared.jitter(),
            drift_pct: shared.correction(),
            starved: starved_now,
            dropped: overflow.saturating_sub(seen_overflow),
            idle: shared.idle(),
        });
        seen_overflow = overflow;

        let report = ControlMsg::Stats(Stats {
            lost_pct: loss,
            jitter_ms: shared.jitter(),
            buffer_ms: jitter_buf_ms + ring_ms,
            late_pct: late as f32,
        });
        if crate::pair::send_secure(&mut control, &channel, &report).is_err() {
            tracing::info!("nadajnik się rozłączył");
            break;
        }
        if stalls > 0 && starved_now > 0 {
            tracing::warn!(stalls, "strumień się zatrzymywał");
        }
    }

    live.store(false, Ordering::Relaxed);
    let _ = crate::pair::send_secure(
        &mut control,
        &channel,
        &ControlMsg::Bye {
            reason: "koniec sesji".into(),
        },
    );
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
    served: &AtomicU64,
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
    let mut playing_since: Option<Instant> = None;
    // Poduszkę podnoszą wyłącznie pakiety spóźnione i puste przebiegi — one
    // znaczą, że czekaliśmy za krótko. Zwykła strata nic o tym nie mówi.
    let mut seen_late = 0u64;
    let mut seen_stalls = 0u64;

    let idle_after = Duration::from_millis(SINK_IDLE_MS);
    let mut last_served = 0u64;
    let mut last_served_at = Instant::now();

    while live.load(Ordering::Relaxed) && running.load(Ordering::Relaxed) {
        // Karta rusza z opóźnieniem rzędu sekundy, a wirtualne wejście czeka,
        // aż ktoś wybierze je w aplikacji — może to być i kwadrans. W obu
        // wypadkach nasz callback milczy i wszystko, co w tym czasie napłynie,
        // jest bezużyteczne: dźwięk sprzed minuty nikogo nie interesuje.
        // Trzymamy więc poduszkę przyciętą do celu, zamiast pozwolić jej
        // spuchnąć do sufitu i zacząć wyrzucać ramki.
        let now_served = served.load(Ordering::Relaxed);
        if now_served != last_served {
            last_served = now_served;
            last_served_at = Instant::now();
        }
        if now_served == 0 || last_served_at.elapsed() > idle_after {
            playing_since = None;
            // Poziom, na którym trzymamy poduszkę w ciszy: cel plus tyle, ile
            // trzeba będzie wlać w pierścień na starcie.
            let hold = start_frames
                + ring_frames(request, device_rate).max(RING_ASSUMED_MS / FRAME_MS as usize);
            let (late, stalls) = {
                let Ok(mut jb) = jitter.lock() else { break };
                jb.trim_to(hold);
                (jb.late, jb.stalls)
            };
            // Cisza po naszej stronie nie jest kłopotem sieci. Bez tego
            // zerowania start odtwarzania rozdymał poduszkę i zostawała taka
            // do końca sesji.
            seen_late = late;
            seen_stalls = stalls;
            if adaptive {
                target.reset(start_frames, Instant::now());
                let wanted = target.frames() as f32 * FRAME_MS as f32;
                drift.set_setpoint(wanted);
                shared.set_setpoint(wanted);
            }
            drift.reset();
            shared.set_correction(0.0);
            shared.set_idle(true);
            primed = false;
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        shared.set_idle(false);
        let playing_for = playing_since.get_or_insert_with(Instant::now).elapsed();

        let ring_target = ring_target(request);

        while producer.occupied_len() < ring_target && producer.vacant_len() >= FRAME_SAMPLES * 2 {
            let popped = {
                let Ok(mut jb) = jitter.lock() else { break };
                // Zabieramy tylko nadwyżkę ponad cel. Poduszka ma zostać na
                // miejscu — to ona daje spóźnionemu pakietowi czas dojść przed
                // swoją kolejką. Opróżnianie jej do dna przy każdym wywołaniu
                // karty kasowało całą ochronę przed przestawieniem i przy
                // okazji zgłaszało pusty bufor jako przestój łącza.
                if jb.playing() && jb.depth() < jb.target_frames() {
                    break;
                }
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

        // Zwykle potok napełnia się w ułamku sekundy; sekunda to bezpiecznik na
        // wypadek, gdyby ujście wołało o więcej, niż zdążyliśmy nazbierać —
        // inaczej utknęlibyśmy w rozruchu i adaptacja nigdy by nie ruszyła.
        if !primed && (occupied >= ring_target || playing_for > Duration::from_secs(1)) {
            primed = true;
            if let Ok(mut jb) = jitter.lock() {
                let dropped = jb.trim_to_target();
                if dropped > 0 {
                    tracing::debug!(dropped, "przycięto poduszkę po napełnieniu potoku");
                }
                // Zanim pierścień się napełnił, pustki w poduszce były w
                // porządku — to trwał rozruch, nie kłopot łącza. Liczniki
                // zaczynają się liczyć dopiero stąd.
                seen_late = jb.late;
                seen_stalls = jb.stalls;
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

            if adaptive && primed && (late > seen_late || stalls > seen_stalls) {
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

/// Zapas skrojony pod rzeczywistą porcję, o jaką woła ujście. Połowa
/// pierścienia to twardy sufit, żeby przy absurdalnym kwancie nie próbować
/// trzymać więcej, niż się mieści.
fn ring_target(request: &AtomicU64) -> usize {
    (request.load(Ordering::Relaxed) as usize * RING_HEADROOM).clamp(RING_MIN, RING_SAMPLES / 2)
}

/// Ten sam zapas wyrażony w ramkach strumienia — pierścień liczy próbki w
/// częstotliwości urządzenia, poduszka liczy ramki po 10 ms.
fn ring_frames(request: &AtomicU64, device_rate: u32) -> usize {
    let ms = ring_target(request) as f32 * 1000.0 / device_rate as f32;
    (ms / FRAME_MS as f32).ceil() as usize
}

/// Liczniki dzielone między wątkami. f32 trzymane w bitach, bo `AtomicF32`
/// nie istnieje w bibliotece standardowej.
#[derive(Default)]
struct SharedStats {
    jitter_ms: AtomicU64,
    correction: AtomicU64,
    setpoint_ms: AtomicU64,
    /// Ujście nie prosi o próbki: albo karta jeszcze nie ruszyła, albo nikt
    /// nie wybrał naszego wirtualnego mikrofonu.
    idle: AtomicBool,
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
    fn set_idle(&self, v: bool) {
        self.idle.store(v, Ordering::Relaxed);
    }
    fn idle(&self) -> bool {
        self.idle.load(Ordering::Relaxed)
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
