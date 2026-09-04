//! Strona nadająca: przechwytuje mikrofon, koduje Opusem i wysyła po UDP.
//!
//! Pacing bierze się z karty dźwiękowej — ramka wychodzi dopiero, gdy callback
//! audio dostarczy materiał. Żadnego własnego zegara.
//!
//! Jeśli urządzenie nie pracuje przy 48 kHz, resampling siedzi tutaj, w wątku
//! sieciowym. Callback audio zostaje pusty: kopiuje próbki do pierścienia i nic
//! poza tym.

use std::io::Write;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use mb_i18n::{t, t1, t2, Key as K};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::HeapRb;

use crate::pair::{establish, recv_secure, send_secure};
use crate::ui::{Reporter, SendStatus};
use mb_engine::{OpusEncoder, VariableResampler};
use mb_proto::{
    ControlMsg, Hello, PayloadKind, RtpHeader, CONTROL_PORT, FRAME_MS, FRAME_SAMPLES,
    PROTOCOL_VERSION, RTP_HEADER_LEN, SAMPLE_RATE,
};

/// Sekunda dźwięku. Gdyby wątek sieciowy się zaciął, wolimy zgubić próbki niż
/// rosnąć w nieskończoność.
const RING_SAMPLES: usize = SAMPLE_RATE as usize;
/// Ile czekamy na odpowiedzi z sieci. Pierwsza przychodzi zwykle w kilkadziesiąt
/// milisekund, ale druga maszyna potrafi odezwać się później — a lista, która
/// zmienia się pod palcami, jest gorsza niż lista, na którą się chwilę czeka.
const DISCOVERY_WINDOW: Duration = Duration::from_millis(2500);

/// Ile próbek naraz podajemy resamplerowi. Wielkość jest dowolna — dzięki temu
/// nie musi dzielić częstotliwości urządzenia bez reszty.
const RESAMPLE_CHUNK: usize = 480;

/// Pseudo-urządzenie: syntetyczny sinus zamiast mikrofonu.
///
/// Pozwala przetestować całą ścieżkę — kodek, sieć, bufor jitter, ujście — na
/// maszynie bez mikrofonu, i daje sygnał o znanym kształcie do sprawdzenia,
/// czy po drugiej stronie nic go nie zniekształca.
pub const TONE_SELECTOR: &str = "tone";
const TONE_HZ: f32 = 440.0;
const TONE_AMPLITUDE: f32 = 0.25; // −12 dBFS: słychać, nie boli

enum Source {
    /// Uchwyt trzymany wyłącznie po to, żeby strumień żył aż do wyjścia z `run`.
    Device(#[allow(dead_code)] mb_audio::CaptureHandle),
    Tone,
}

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
            deadline = Instant::now();
        }
    }
}

/// Diagnostyczne gubienie pakietów tuż przed wysłaniem.
///
/// `tc netem` jest linuksowy, a sprawdzić trzeba obie strony — także wtedy, gdy
/// nadajnik stoi na Windows. Ziarno jest stałe, więc przebieg da się powtórzyć.
struct Dropper {
    threshold: u64,
    state: u64,
    pub dropped: u64,
}

impl Dropper {
    fn new(pct: f32) -> Self {
        Self {
            threshold: (pct.clamp(0.0, 100.0) as f64 / 100.0 * u64::MAX as f64) as u64,
            state: 0x2545_F491_4F6C_DD1D,
            dropped: 0,
        }
    }

    fn should_drop(&mut self) -> bool {
        if self.threshold == 0 {
            return false;
        }
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let drop = self.state < self.threshold;
        if drop {
            self.dropped += 1;
        }
        drop
    }
}

/// Jak ma pracowac nadajnik.
#[derive(Debug, Clone)]
pub struct Options {
    /// Odbiornik: adres, `adres:port` albo nazwa z listy. `None` znaczy
    /// "znajdz jedyny w sieci".
    pub to: Option<String>,
    /// Zrodlo: `default`, `@3`, fragment nazwy albo `tone`.
    pub device: String,
    pub gain_db: f32,
    pub bitrate: u32,
    /// Diagnostyka: ile procent pakietow gubic celowo.
    pub drop_pct: f32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            to: None,
            device: "default".into(),
            gain_db: 0.0,
            bitrate: 24_000,
            drop_pct: 0.0,
        }
    }
}

pub fn run(opts: &Options, ui: &dyn Reporter, running: Arc<AtomicBool>) -> Result<()> {
    let (device, bitrate, drop_pct) = (opts.device.as_str(), opts.bitrate, opts.drop_pct);
    let control_addr = match opts.to.as_deref() {
        Some(target) => target_addr(target, ui)?,
        None => sole_peer(ui)?,
    };
    let gain = 10f32.powf(opts.gain_db / 20.0);

    // 1. Mikrofon najpierw — bez sensu zawracać głowę drugiej maszynie, jeśli
    //    lokalne urządzenie i tak nie działa.
    let rb = HeapRb::<f32>::new(RING_SAMPLES);
    let (mut producer, mut consumer) = rb.split();
    let overruns = Arc::new(AtomicU64::new(0));

    let (_source, source_name, device_rate) = if device.eq_ignore_ascii_case(TONE_SELECTOR) {
        let running = Arc::clone(&running);
        std::thread::spawn(move || generate_tone(&mut producer, &running));
        (
            Source::Tone,
            format!("{TONE_SELECTOR} — generator 440 Hz"),
            SAMPLE_RATE,
        )
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
        let rate = capture.sample_rate;
        (Source::Device(capture), name, rate)
    };

    // Urządzenie nie musi mieć 48 kHz; różnicę zdejmuje resampler.
    let mut resampler = if device_rate == SAMPLE_RATE {
        None
    } else {
        ui.line(&t2(K::SesConversionSend, device_rate, SAMPLE_RATE));
        Some(VariableResampler::new(
            SAMPLE_RATE as f64 / device_rate as f64,
            RESAMPLE_CHUNK,
        )?)
    };

    let mut encoder = OpusEncoder::new(bitrate)?;

    // 2. Uzgodnienie po TCP.
    let mut control = TcpStream::connect(control_addr)
        .with_context(|| format!("nie mogę połączyć się z {control_addr}"))?;
    control.set_nodelay(true)?;

    // Od tego miejsca kanał jest zaszyfrowany. Uzgodnienie może po drodze
    // poprosić o kod parowania.
    let channel = Arc::new(Mutex::new(establish(&mut control, ui)?));

    send_secure(
        &mut control,
        &channel,
        &ControlMsg::Hello(Hello {
            version: PROTOCOL_VERSION,
            payload: PayloadKind::Opus,
            sample_rate: SAMPLE_RATE,
            channels: 1,
            frame_ms: FRAME_MS,
            device: source_name.clone(),
            host: mb_net::hostname(),
        }),
    )?;

    let accept = match recv_secure(&mut control, &channel)? {
        ControlMsg::Accept(a) => a,
        ControlMsg::Reject { reason } => bail!("{}", t1(K::ErrRejected, reason)),
        other => bail!("nieoczekiwana odpowiedź na HELLO: {other:?}"),
    };
    let cipher = mb_net::MediaCipher::new(&accept.media_key)?;
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

    // 3. Statystyki z odbiornika czyta osobny wątek; ta sama wartość steruje
    //    tym, ile bitów koder przeznacza na FEC.
    let reported_loss = Arc::new(AtomicU64::new(0));
    // Odbiornik zna opoznienie i jitter, nadajnik nie ma jak ich zmierzyc
    // sam - pokazujemy wiec u siebie to, co przyszlo w statystykach.
    let reported_latency = Arc::new(AtomicU64::new(0));
    let reported_jitter = Arc::new(AtomicU64::new(0));
    {
        let running = Arc::clone(&running);
        let reported_loss = Arc::clone(&reported_loss);
        let reported_latency = Arc::clone(&reported_latency);
        let reported_jitter = Arc::clone(&reported_jitter);
        let mut reader = control.try_clone()?;
        let channel = Arc::clone(&channel);
        std::thread::spawn(move || {
            while running.load(Ordering::Relaxed) {
                match recv_secure(&mut reader, &channel) {
                    Ok(ControlMsg::Stats(s)) => {
                        reported_loss.store(s.lost_pct.to_bits() as u64, Ordering::Relaxed);
                        reported_latency.store(s.buffer_ms.to_bits() as u64, Ordering::Relaxed);
                        reported_jitter.store(s.jitter_ms.to_bits() as u64, Ordering::Relaxed);
                        tracing::debug!(
                            strat = format!("{:.1}%", s.lost_pct),
                            jitter = format!("{:.1} ms", s.jitter_ms),
                            bufor = format!("{:.0} ms", s.buffer_ms),
                            "odbiornik"
                        );
                    }
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

    // 4. Pętla wysyłkowa.
    let mut device_chunk = vec![0f32; RESAMPLE_CHUNK];
    // Próbki już w dziedzinie 48 kHz, czekające na złożenie w pełne ramki.
    let mut pending: Vec<f32> = Vec::with_capacity(FRAME_SAMPLES * 4);
    let mut pcm = vec![0i16; FRAME_SAMPLES];
    let mut packet = vec![0u8; RTP_HEADER_LEN + 1500];

    let mut seq: u16 = 0;
    let mut ext_seq: u64 = 0;
    let mut timestamp: u32 = 0;
    let mut sent: u64 = 0;
    let mut bytes: u64 = 0;
    let mut peak = 0f32;
    let mut last_report = Instant::now();

    let mut dropper = Dropper::new(drop_pct);
    if drop_pct > 0.0 {
        ui.line(&t1(K::SesDroppingOnPurpose, drop_pct));
    }
    ui.line(&t1(K::SesOpusAt, bitrate / 1000));

    while running.load(Ordering::Relaxed) {
        if consumer.occupied_len() < RESAMPLE_CHUNK {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        consumer.pop_slice(&mut device_chunk);

        match resampler.as_mut() {
            Some(r) => r.process(&device_chunk, &mut pending)?,
            None => pending.extend_from_slice(&device_chunk),
        }

        while pending.len() >= FRAME_SAMPLES {
            for (dst, &src) in pcm.iter_mut().zip(pending.iter()) {
                let v = (src * gain).clamp(-1.0, 1.0);
                peak = peak.max(v.abs());
                *dst = (v * 32767.0) as i16;
            }
            pending.drain(..FRAME_SAMPLES);

            // Nagłówek powstaje przed szyfrowaniem, bo wchodzi w uwierzytelnienie:
            // podmieniony numer sekwencyjny unieważnia pakiet po drugiej stronie.
            RtpHeader {
                marker: sent == 0,
                payload: PayloadKind::Opus,
                seq,
                timestamp,
                ssrc: accept.ssrc,
            }
            .encode_into(&mut packet)?;

            let payload_len = {
                let encoded = encoder.encode(&pcm)?;
                let sealed = cipher.seal(ext_seq, &packet[..RTP_HEADER_LEN], encoded)?;
                packet[RTP_HEADER_LEN..RTP_HEADER_LEN + sealed.len()].copy_from_slice(&sealed);
                sealed.len()
            };

            if !dropper.should_drop() {
                if let Err(e) = socket.send(&packet[..RTP_HEADER_LEN + payload_len]) {
                    tracing::warn!(error = %e, "nie udało się wysłać ramki");
                }
            }

            seq = seq.wrapping_add(1);
            // Wartość jednorazowa szyfrowania nie może się powtórzyć, więc
            // liczymy pakiety w pełnych 64 bitach, nie w szesnastu z nagłówka.
            ext_seq += 1;
            timestamp = timestamp.wrapping_add(FRAME_SAMPLES as u32);
            sent += 1;
            bytes += (RTP_HEADER_LEN + payload_len) as u64;
        }

        if last_report.elapsed() >= Duration::from_secs(1) {
            let elapsed = last_report.elapsed().as_secs_f64();
            last_report = Instant::now();

            // Ile strat widzi odbiornik, tyle redundancji ma dokładać koder.
            let loss = f32::from_bits(reported_loss.load(Ordering::Relaxed) as u32);
            encoder.set_expected_loss(loss)?;

            let lost_input = overruns.swap(0, Ordering::Relaxed);
            if lost_input > 0 {
                tracing::warn!(
                    probki = lost_input,
                    "bufor wejściowy się przepełnił — wątek sieciowy nie nadąża"
                );
            }

            ui.send_status(&SendStatus {
                frames: sent,
                kbps: (bytes as f64 * 8.0 / elapsed / 1000.0) as f32,
                peak_dbfs: mb_audio::peak_dbfs(&[peak]),
                loss_pct: loss,
                latency_ms: f32::from_bits(reported_latency.load(Ordering::Relaxed) as u32),
                jitter_ms: f32::from_bits(reported_jitter.load(Ordering::Relaxed) as u32),
                fec_pct: encoder.expected_loss(),
                overruns: lost_input,
                dropped_on_purpose: dropper.dropped,
            });
            peak = 0.0;
            bytes = 0;
        }
    }

    let _ = ControlMsg::Bye {
        reason: "użytkownik przerwał".into(),
    }
    .write_to(&mut control);
    let _ = control.flush();
    ui.line(&t1(K::SesDone, sent));
    Ok(())
}

/// `host` albo `host:port`; bez portu dokleja domyślny.
fn resolve(target: &str, default_port: u16) -> Result<SocketAddr> {
    let with_port = if has_port(target) {
        target.to_string()
    } else {
        format!("{target}:{default_port}")
    };

    with_port
        .to_socket_addrs()
        .with_context(|| format!("nie umiem rozwiązać adresu `{target}`"))?
        .next()
        .ok_or_else(|| anyhow!("{}", t1(K::ErrTargetNothing, target)))
}

/// Rozstrzyga, czy użytkownik podał port.
///
/// Goły adres IPv6 sam w sobie jest pełen dwukropków, więc dla niego portem
/// jest wyłącznie forma `[adres]:port`.
fn has_port(target: &str) -> bool {
    if target.starts_with('[') {
        target.contains("]:")
    } else {
        target.matches(':').count() == 1
    }
}

/// Zamienia to, co użytkownik wpisał w `--to`, na adres.
///
/// Najpierw jako adres, bo tak jest bez czekania. Dopiero gdy to nie wyjdzie,
/// szukamy w sieci maszyny o takiej nazwie — `--to salon` ma działać tak samo
/// jak `--to 192.168.1.40`, skoro nazwę widać na liście z `discover`.
fn target_addr(target: &str, ui: &dyn Reporter) -> Result<SocketAddr> {
    match resolve(target, CONTROL_PORT) {
        Ok(addr) => Ok(addr),
        Err(e) => match peer_by_name(target, ui)? {
            Some(peer) => {
                ui.line(&t2(K::SesPeerIs, &peer.name, peer.addr));
                Ok(peer.addr)
            }
            None => Err(e),
        },
    }
}

fn peer_by_name(fragment: &str, ui: &dyn Reporter) -> Result<Option<mb_net::Peer>> {
    let needle = fragment.to_lowercase();
    Ok(discover(ui)?
        .into_iter()
        .find(|p| p.name.to_lowercase().contains(&needle)))
}

/// Znajduje jedyny odbiornik w sieci albo tłumaczy, czego brakuje.
fn sole_peer(ui: &dyn Reporter) -> Result<SocketAddr> {
    let peers = discover(ui)?;
    match peers.len() {
        0 => bail!("{}", t(K::ErrNoReceiver)),
        1 => {
            let peer = peers.into_iter().next().expect("jeden jest");
            ui.line(&t2(K::SesFound, &peer.name, peer.addr));
            Ok(peer.addr)
        }
        _ => {
            ui.line(t(K::SesSeveral));
            for peer in &peers {
                ui.line(&format!("  {:<24} {}", peer.name, peer.addr));
            }
            bail!("{}", t(K::ErrPickOne));
        }
    }
}

/// Przeszukuje sieć i odsiewa to, z czym i tak byśmy się nie dogadali.
fn discover(ui: &dyn Reporter) -> Result<Vec<mb_net::Peer>> {
    ui.line(t(K::SesSearching));
    let peers = mb_net::browse(DISCOVERY_WINDOW)?;
    let (ok, obce): (Vec<_>, Vec<_>) = peers.into_iter().partition(mb_net::Peer::compatible);
    for peer in obce {
        ui.line(&mb_i18n::t3(
            K::SesSkipping,
            &peer.name,
            peer.version,
            PROTOCOL_VERSION,
        ));
    }
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dropper_hits_close_to_the_requested_rate() {
        let mut d = Dropper::new(5.0);
        for _ in 0..100_000 {
            d.should_drop();
        }
        let rate = d.dropped as f64 / 1000.0;
        assert!((rate - 5.0).abs() < 0.4, "gubi {rate:.2}%");
    }

    #[test]
    fn zero_percent_drops_nothing() {
        let mut d = Dropper::new(0.0);
        for _ in 0..10_000 {
            assert!(!d.should_drop());
        }
    }

    #[test]
    fn a_bare_host_gets_the_default_port() {
        assert_eq!(resolve("127.0.0.1", 47100).unwrap().port(), 47100);
    }

    #[test]
    fn an_explicit_port_wins() {
        assert_eq!(resolve("127.0.0.1:9000", 47100).unwrap().port(), 9000);
    }

    #[test]
    fn ipv6_needs_brackets_to_carry_a_port() {
        // Bare IPv6 is all colons; only the bracketed form names a port.
        assert!(!has_port("::1"));
        assert!(!has_port("fe80::1"));
        assert!(has_port("[::1]:47100"));
        assert!(!has_port("[::1]"));
        assert_eq!(resolve("[::1]:9000", 47100).unwrap().port(), 9000);
        assert_eq!(resolve("::1", 47100).unwrap().port(), 47100);
    }
}
