//! Deterministic network impairment, for driving the engine in tests.
//!
//! `tc netem` is the real instrument, but it needs two machines and a Linux
//! kernel. This gives the same three defects — loss, variable delay, and the
//! reordering that variable delay produces on its own — from a seeded
//! generator, so a failure is reproducible and an eight-hour run takes seconds.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// Small, fast, and reproducible across platforms and Rust versions, which
/// `rand` deliberately is not.
#[derive(Debug)]
struct Lcg(u64);

impl Lcg {
    fn next_u64(&mut self) -> u64 {
        // Numerical Recipes constants.
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        self.0
    }

    /// Uniform in [0, 1).
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Queued {
    deliver_at_us: u64,
    seq: u64,
    packet: Vec<u8>,
}

#[derive(Debug)]
pub struct NetSim {
    rng: Lcg,
    loss: f64,
    base_delay_us: u64,
    jitter_us: u64,
    queue: BinaryHeap<Reverse<Queued>>,
    pub sent: u64,
    pub dropped: u64,
    pub delivered: u64,
    pub reordered: u64,
    last_delivered_seq: Option<u64>,
}

impl NetSim {
    pub fn new(seed: u64, loss_pct: f64, base_delay_ms: f64, jitter_ms: f64) -> Self {
        Self {
            rng: Lcg(seed.wrapping_mul(2_862_933_555_777_941_757).wrapping_add(3_037_000_493)),
            loss: loss_pct / 100.0,
            base_delay_us: (base_delay_ms * 1000.0) as u64,
            jitter_us: (jitter_ms * 1000.0) as u64,
            queue: BinaryHeap::new(),
            sent: 0,
            dropped: 0,
            delivered: 0,
            reordered: 0,
            last_delivered_seq: None,
        }
    }

    /// A perfect link: no loss, no jitter, fixed delay.
    pub fn perfect(base_delay_ms: f64) -> Self {
        Self::new(1, 0.0, base_delay_ms, 0.0)
    }

    pub fn send(&mut self, seq: u64, packet: Vec<u8>, now_us: u64) {
        self.sent += 1;
        if self.rng.next_f64() < self.loss {
            self.dropped += 1;
            return;
        }
        let extra = if self.jitter_us == 0 {
            0
        } else {
            (self.rng.next_f64() * self.jitter_us as f64) as u64
        };
        self.queue.push(Reverse(Queued {
            deliver_at_us: now_us + self.base_delay_us + extra,
            seq,
            packet,
        }));
    }

    /// Everything whose delivery time has arrived, in the order it arrives —
    /// which is not send order whenever jitter exceeds the packet interval.
    pub fn poll(&mut self, now_us: u64) -> Vec<(u64, Vec<u8>)> {
        let mut out = Vec::new();
        while let Some(Reverse(head)) = self.queue.peek() {
            if head.deliver_at_us > now_us {
                break;
            }
            let Reverse(q) = self.queue.pop().expect("peeked");
            if let Some(last) = self.last_delivered_seq {
                if q.seq < last {
                    self.reordered += 1;
                }
            }
            self.last_delivered_seq = Some(self.last_delivered_seq.map_or(q.seq, |l| l.max(q.seq)));
            self.delivered += 1;
            out.push((q.seq, q.packet));
        }
        out
    }

    pub fn in_flight(&self) -> usize {
        self.queue.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_perfect_link_delivers_everything_in_order_after_the_delay() {
        let mut net = NetSim::perfect(5.0);
        for seq in 0..10u64 {
            net.send(seq, vec![seq as u8], seq * 10_000);
        }
        assert!(net.poll(4_000).is_empty(), "za wcześnie");
        let got = net.poll(200_000);
        assert_eq!(got.len(), 10);
        assert!(got.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(net.dropped, 0);
        assert_eq!(net.reordered, 0);
    }

    #[test]
    fn loss_rate_lands_near_the_requested_figure() {
        let mut net = NetSim::new(42, 5.0, 1.0, 0.0);
        for seq in 0..20_000u64 {
            net.send(seq, vec![0], seq * 10_000);
        }
        let rate = net.dropped as f64 * 100.0 / net.sent as f64;
        assert!((rate - 5.0).abs() < 0.6, "strata {rate:.2}%");
    }

    #[test]
    fn jitter_beyond_the_packet_interval_reorders() {
        // 30 ms of jitter on a 10 ms packet spacing has to overtake.
        let mut net = NetSim::new(7, 0.0, 5.0, 30.0);
        for seq in 0..500u64 {
            net.send(seq, vec![0], seq * 10_000);
        }
        let mut order = Vec::new();
        for t in 0..600u64 {
            order.extend(net.poll(t * 10_000).into_iter().map(|(s, _)| s));
        }
        assert_eq!(order.len(), 500);
        assert!(net.reordered > 20, "przestawień: {}", net.reordered);
    }

    #[test]
    fn the_same_seed_gives_the_same_link() {
        let run = || {
            let mut net = NetSim::new(99, 3.0, 2.0, 8.0);
            for seq in 0..2_000u64 {
                net.send(seq, vec![0], seq * 10_000);
            }
            (net.dropped, net.reordered)
        };
        assert_eq!(run(), run());
    }
}
