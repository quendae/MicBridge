//! Strona nadająca: przechwytuje mikrofon i wysyła go po UDP.
//!
//! Pacing bierze się z karty dźwiękowej — wątek sieciowy wysyła ramkę dopiero,
//! gdy callback audio dostarczy 480 próbek. Żadnego własnego zegara.

use std::io::Write;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::HeapRb;

use mb_proto::{
    rtp::encode_pcm, ControlMsg, Hello, PayloadKind, RtpHeader, CONTROL_PORT, FRAME_MS,
    FRAME_SAMPLES, PROTOCOL_VERSION, RTP_HEADER_LEN, SAMPLE_RATE,
};

/// Sekunda dźwięku. Gdyby wątek sieciowy się zaciął, wolimy nadpisać stare
/// próbki niż rosnąć w nieskończoność.
const RING_SAMPLES: usize = SAMPLE_RATE as usize;

/// Pseudo-urządzenie: syntetyczny sinus zamiast mikrofonu.
///
/// Pozwala przetestować całą ścieżkę — ramkowanie, sieć, bufor jitter, ujście —
/// na maszynie bez mikrofonu, i daje sygnał o znanym kształcie do sprawdzenia,
/// czy po drugiej stronie nic go nie zniekształca.
pub const TONE_SELECTOR: &str = "tone";
const TONE_HZ: f32 = 440.0;
const TONE_AMPLITUDE: f32 = 0.25; // −12 dBFS: słychać, nie boli

enum Source {
    /// Uchwyt trzymany wyłącznie po to, żeby strumień żył aż do wyjścia z `run`.
    Device(#[allow(dead_code)] mb_audio::CaptureHandle),
    Tone,
}

/// Wypełnia pierścień sinusem, taktowany bezwzględnymi terminami, żeby drobne
/// spóźnienia się nie kumulowały.
fn generate_tone<P: Producer<Item = f32>>(producer: &mut P, running: &AtomicBool) {
    let step = std::f32::consts::TAU * TONE_HZ / SAMPLE_RATE as f32;
    let mut phase = 0f32;
    let mut buf = vec![0f32; FRAME_SAMPLES];
    let mut deadline = Instant::now();

    while running.load(Ordering::Relaxed) {
        for s in buf.iter_mut() {
            *s = phase.sin() * TONE_AMPLITUDE;
            phase += step;
            if phase > std::f32::consts::TAU {
                phase -= std::f32::consts::TAU;
            }
        }
        producer.push_slice(&buf);

        deadline += Duration::from_millis(FRAME_MS as u64);
        if let Some(wait) = deadline.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        } else {
            // Spóźnieni: nie próbujemy nadrabiać, tylko przesuwamy termin.
            deadline = Instant::now();
        }
    }
}

pub fn run(to: &str, device: &str, gain_db: f32) -> Result<()> {
    let control_addr = resolve(to, CONTROL_PORT)?;
    let gain = 10f32.powf(gain_db / 20.0);

    // 1. Mikrofon najpierw — bez sensu zawracać głowę drugiej maszynie,
    //    jeśli lokalne urządzenie i tak nie działa.
    let rb = HeapRb::<f32>::new(RING_SAMPLES);
    let (mut producer, mut consumer) = rb.split();
    let overruns = Arc::new(AtomicU64::new(0));
    let running = Arc::new(AtomicBool::new(true));

    // `_source` trzyma strumień przy życiu; upuszczenie go zatrzymuje dźwięk.
    let (_source, source_name) = if device.eq_ignore_ascii_case(TONE_SELECTOR) {
        let running = Arc::clone(&running);
        std::thread::spawn(move || generate_tone(&mut producer, &running));
        (Source::Tone, format!("{TONE_SELECTOR} — generator 440 Hz"))
    } else {
        let overruns = Arc::clone(&overruns);
        let capture = mb_audio::start_capture(device, SAMPLE_RATE, move |mono| {
            let written = producer.push_slice(mono);
            if written < mono.len() {
                overruns.fetch_add((mono.len() - written) as u64, Ordering::Relaxed);
            }
        })?;
        tracing::info!(
            device = %capture.device_name,
            channels = capture.channels,
            rate = capture.sample_rate,
            "mikrofon otwarty"
        );
        let name = capture.device_name.clone();
        (Source::Device(capture), name)
    };

    // 2. Uzgodnienie po TCP.
    let mut control = TcpStream::connect(control_addr)
        .with_context(|| format!("nie mogę połączyć się z {control_addr}"))?;
    control.set_nodelay(true)?;

    let hello = Hello {
        version: PROTOCOL_VERSION,
        payload: PayloadKind::PcmS16,
        sample_rate: SAMPLE_RATE,
        channels: 1,
        frame_ms: FRAME_MS,
        device: source_name.clone(),
        host: hostname(),
    };
    ControlMsg::Hello(hello).write_to(&mut control)?;

    let accept = match ControlMsg::read_from(&mut control)? {
        ControlMsg::Accept(a) => a,
        ControlMsg::Reject { reason } => bail!("odbiornik odrzucił połączenie: {reason}"),
        other => bail!("nieoczekiwana odpowiedź na HELLO: {other:?}"),
    };
    if accept.version != PROTOCOL_VERSION {
        bail!(
            "niezgodna wersja protokołu: my {PROTOCOL_VERSION}, odbiornik {}",
            accept.version
        );
    }

    let media_addr = SocketAddr::new(control_addr.ip(), accept.media_port);
    tracing::info!(
        host = %accept.host,
        sink = %accept.sink,
        %media_addr,
        ssrc = format!("{:08x}", accept.ssrc),
        "połączono"
    );

    let socket = UdpSocket::bind(if media_addr.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    })?;
    socket.connect(media_addr)?;

    // 3. Statystyki z odbiornika przychodzą własnym wątkiem, żeby nie blokować
    //    ścieżki wysyłkowej na odczycie z gniazda.
    {
        let running = Arc::clone(&running);
        let mut reader = control.try_clone()?;
        std::thread::spawn(move || {
            while running.load(Ordering::Relaxed) {
                match ControlMsg::read_from(&mut reader) {
                    Ok(ControlMsg::Stats(s)) => tracing::info!(
                        strat = format!("{:.1}%", s.lost_pct),
                        jitter = format!("{:.1} ms", s.jitter_ms),
                        bufor = format!("{:.0} ms", s.buffer_ms),
                        "odbiornik"
                    ),
                    Ok(ControlMsg::Bye { reason }) => {
                        tracing::warn!(%reason, "odbiornik zakończył sesję");
                        running.store(false, Ordering::Relaxed);
                        return;
                    }
                    Ok(other) => tracing::debug!(?other, "sterowanie"),
                    Err(e) => {
                        if running.load(Ordering::Relaxed) {
                            tracing::warn!(error = %e, "kanał sterujący zerwany");
                        }
                        running.store(false, Ordering::Relaxed);
                        return;
                    }
                }
            }
        });
    }

    {
        let running = Arc::clone(&running);
        ctrlc::set_handler(move || running.store(false, Ordering::Relaxed))
            .context("nie mogę przechwycić Ctrl-C")?;
    }

    // 4. Pętla wysyłkowa.
    let mut frame = vec![0f32; FRAME_SAMPLES];
    let mut pcm = vec![0i16; FRAME_SAMPLES];
    let mut packet = vec![0u8; RTP_HEADER_LEN + FRAME_SAMPLES * 2];

    let mut seq: u16 = 0;
    let mut timestamp: u32 = 0;
    let mut sent: u64 = 0;
    let mut peak = 0f32;
    let mut last_report = Instant::now();

    println!("Nadaję. Ctrl-C kończy.");

    while running.load(Ordering::Relaxed) {
        if consumer.occupied_len() < FRAME_SAMPLES {
            // Karta dźwiękowa jeszcze nie dostarczyła pełnej ramki.
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        consumer.pop_slice(&mut frame);

        for (dst, &src) in pcm.iter_mut().zip(frame.iter()) {
            let v = (src * gain).clamp(-1.0, 1.0);
            peak = peak.max(v.abs());
            *dst = (v * 32767.0) as i16;
        }

        let header = RtpHeader {
            marker: sent == 0,
            payload: PayloadKind::PcmS16,
            seq,
            timestamp,
            ssrc: accept.ssrc,
        };
        header.encode_into(&mut packet)?;
        let n = encode_pcm(&pcm, &mut packet[RTP_HEADER_LEN..]);

        if let Err(e) = socket.send(&packet[..RTP_HEADER_LEN + n]) {
            tracing::warn!(error = %e, "nie udało się wysłać ramki");
        }

        seq = seq.wrapping_add(1);
        timestamp = timestamp.wrapping_add(FRAME_SAMPLES as u32);
        sent += 1;

        if last_report.elapsed() >= Duration::from_secs(1) {
            let lost_input = overruns.swap(0, Ordering::Relaxed);
            if lost_input > 0 {
                tracing::warn!(
                    probki = lost_input,
                    "bufor wejściowy się przepełnił — wątek sieciowy nie nadąża"
                );
            }
            println!(
                "  wysłano {sent:>6} ramek   szczyt {:>6.1} dBFS",
                mb_audio::peak_dbfs(&[peak])
            );
            peak = 0.0;
            last_report = Instant::now();
        }
    }

    let _ = ControlMsg::Bye {
        reason: "użytkownik przerwał".into(),
    }
    .write_to(&mut control);
    let _ = control.flush();
    println!("Zakończono. Wysłano {sent} ramek.");
    Ok(())
}

/// `host` albo `host:port`; bez portu dokleja domyślny.
fn resolve(target: &str, default_port: u16) -> Result<SocketAddr> {
    let with_port = if target.contains(':') && !target.starts_with('[') {
        // IPv6 bez nawiasów potraktowalibyśmy tu błędnie, ale wtedy i tak
        // wymagamy formy [::1]:47100.
        target.to_string()
    } else if target.starts_with('[') && target.contains("]:") {
        target.to_string()
    } else {
        format!("{target}:{default_port}")
    };

    with_port
        .to_socket_addrs()
        .with_context(|| format!("nie umiem rozwiązać adresu `{target}`"))?
        .next()
        .ok_or_else(|| anyhow!("adres `{target}` nie wskazuje na nic"))
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "nieznany".into())
}
