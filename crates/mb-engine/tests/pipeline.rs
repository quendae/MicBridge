//! End-to-end runs of the engine over a simulated link.
//!
//! Nothing here touches a sound card or a socket: the encoder feeds `NetSim`,
//! the jitter buffer reorders what survives, and the decoder rebuilds the rest.
//! That is the whole point of keeping `mb-engine` free of the operating system
//! — an eight-hour session runs in a second, and a failure is reproducible from
//! its seed.

use mb_engine::codec::frame_buffer;
use mb_engine::{
    netsim::NetSim, AdaptiveTarget, DriftController, JitterBuffer, OpusDecoder, OpusEncoder, Pop,
};
use mb_proto::{FRAME_MS, FRAME_SAMPLES, SAMPLE_RATE};

const FRAME_US: u64 = (FRAME_MS as u64) * 1000;

fn speech_like(frame_index: usize) -> Vec<i16> {
    // A sweep rather than a fixed tone: a constant sine flatters concealment,
    // because extrapolating it is trivial.
    let base = 200.0 + 300.0 * ((frame_index as f32 / 50.0).sin() + 1.0);
    let start = frame_index * FRAME_SAMPLES;
    (0..FRAME_SAMPLES)
        .map(|i| {
            let t = (start + i) as f32 / SAMPLE_RATE as f32;
            ((std::f32::consts::TAU * base * t).sin() * 8000.0) as i16
        })
        .collect()
}

fn rms(samples: &[i16]) -> f64 {
    (samples.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
}

struct Outcome {
    frames_out: usize,
    silent_frames: usize,
    recovered: u64,
    concealed: u64,
    stalls: u64,
    lost_pct: f32,
}

/// Encode, ship, reorder, decode. `seconds` of audio at 100 frames per second.
fn run_link(seconds: u64, net: &mut NetSim, target_frames: usize) -> Outcome {
    let mut enc = OpusEncoder::new(24_000).unwrap();
    let mut dec = OpusDecoder::new().unwrap();
    let mut jb = JitterBuffer::new(target_frames, 40);
    let mut out = frame_buffer();

    let total = seconds as usize * (1000 / FRAME_MS as usize);
    let mut frames_out = 0;
    let mut silent_frames = 0;
    let mut concealed = 0u64;

    for i in 0..total {
        let now = i as u64 * FRAME_US;
        let packet = enc.encode(&speech_like(i)).unwrap().to_vec();
        net.send(i as u64, packet, now);

        for (s, p) in net.poll(now) {
            jb.push(s, p);
        }

        // One frame consumed per frame period, as the sound card would.
        match jb.pop() {
            Pop::Packet(p) => {
                dec.decode(&p, &mut out).unwrap();
                frames_out += 1;
            }
            Pop::LostRecoverable(next) => {
                dec.decode_fec(&next, &mut out).unwrap();
                frames_out += 1;
            }
            Pop::Lost => {
                dec.conceal(&mut out).unwrap();
                concealed += 1;
                frames_out += 1;
            }
            Pop::Filling => continue,
        }

        // Only judge once the pipeline is primed and the encoder has settled.
        if frames_out > 20 && rms(&out) < 1.0 {
            silent_frames += 1;
        }
    }

    Outcome {
        frames_out,
        silent_frames,
        recovered: jb.recovered,
        concealed,
        stalls: jb.stalls,
        lost_pct: jb.loss_pct(),
    }
}

#[test]
fn a_clean_link_delivers_every_frame() {
    let mut net = NetSim::perfect(1.0);
    let r = run_link(30, &mut net, 3);
    assert_eq!(r.stalls, 0);
    assert_eq!(r.lost_pct, 0.0);
    assert_eq!(r.silent_frames, 0);
    assert!(r.frames_out > 2_900, "wypuszczono {} ramek", r.frames_out);
}

/// The milestone-2 exit criterion: 2% loss must not produce audible holes.
#[test]
fn two_percent_loss_is_repaired_without_gaps() {
    let mut net = NetSim::new(2024, 2.0, 2.0, 4.0);
    let r = run_link(60, &mut net, 3);

    assert!(
        (r.lost_pct - 2.0).abs() < 1.0,
        "bufor zgłasza {:.2}% strat, sieć gubiła 2%",
        r.lost_pct
    );
    assert!(
        r.recovered > 0,
        "przy 2% strat FEC musi się do czegoś przydać"
    );
    // Isolated losses dominate at 2%, so almost everything goes through FEC
    // rather than concealment.
    assert!(
        r.recovered > r.concealed * 4,
        "za dużo ukrywania ({}) wobec odtwarzania z FEC ({})",
        r.concealed,
        r.recovered
    );
    assert_eq!(r.silent_frames, 0, "żadna ramka nie może wyjść jako cisza");
    assert_eq!(r.stalls, 0, "2% strat nie ma prawa zatrzymać strumienia");
}

#[test]
fn jitter_that_reorders_packets_does_not_break_the_stream() {
    // 25 ms of jitter on 10 ms spacing: packets routinely overtake each other.
    let mut net = NetSim::new(7, 0.0, 5.0, 25.0);
    let r = run_link(30, &mut net, 4);
    assert!(net.reordered > 100, "przestawień: {}", net.reordered);
    assert_eq!(r.silent_frames, 0);
    assert!(
        r.lost_pct < 1.0,
        "bufor 40 ms powinien wchłonąć 25 ms jittera, zgubił {:.2}%",
        r.lost_pct
    );
}

/// Eight hours of drift, without the codec — this is a control-loop question,
/// and encoding 2.9 million frames would take minutes to prove nothing extra.
#[test]
fn eight_hours_of_clock_drift_does_not_move_the_latency() {
    for drift_ppm in [-50.0, -30.0, 0.0, 30.0, 50.0] {
        let mut ctl = DriftController::new(30.0);
        let dt = 0.02_f64;
        let mut depth = 30.0_f64;
        let mut worst = 0.0_f64;

        for step in 0..(8.0 * 3600.0 / dt) as usize {
            let c = ctl.update(depth as f32, dt as f32);
            depth += 1000.0 * (drift_ppm * 1e-6 + c) * dt;
            assert!(depth > 0.0, "niedomiar przy dryfie {drift_ppm} ppm");
            if step > 5_000 {
                worst = worst.max((depth - 30.0).abs());
            }
        }
        assert!(
            worst < 3.0,
            "przy {drift_ppm} ppm bufor odjechał o {worst:.1} ms przez osiem godzin"
        );
    }
}

/// Without the controller the same drift is a session-ruining defect. This test
/// exists so the previous one cannot pass by accident.
#[test]
fn the_same_drift_uncorrected_would_ruin_the_session() {
    let mut depth = 30.0_f64;
    for _ in 0..(8.0 * 3600.0 / 0.02) as usize {
        depth += 1000.0 * 30e-6 * 0.02;
    }
    assert!(
        depth > 800.0,
        "kontrola założeń: bez regulatora +30 ppm dokłada blisko sekundę, wyszło {depth:.0} ms"
    );
}

#[test]
fn a_burst_of_lateness_widens_the_cushion_then_it_narrows_again() {
    use std::time::{Duration, Instant};

    let t0 = Instant::now();
    let mut target = AdaptiveTarget::new(3, 2, 12).with_settle(Duration::from_secs(5));
    let mut jb = JitterBuffer::new(target.frames(), 40);

    // A bad couple of seconds on the Wi-Fi: packets keep arriving too late.
    for i in 0..4 {
        target.on_late(t0 + Duration::from_millis(i * 600));
    }
    jb.set_target_frames(target.frames());
    assert_eq!(jb.target_frames(), 11);

    // Then it calms down and the cushion is given back, one frame at a time.
    let mut now = t0 + Duration::from_secs(2);
    for _ in 0..20 {
        now += Duration::from_secs(5);
        if target.tick(now) {
            jb.set_target_frames(target.frames());
        }
    }
    assert_eq!(jb.target_frames(), 2, "spokojne łącze wraca do minimum");
}
