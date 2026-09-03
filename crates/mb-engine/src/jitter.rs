//! Reordering buffer that absorbs network jitter.
//!
//! It holds *encoded* packets, not decoded audio. That is deliberate: Opus
//! recovers a lost frame from the redundant copy carried inside the following
//! packet, so decoding has to happen after reordering, with the successor in
//! hand. Decoding on arrival would throw that away.
//!
//! Steady-state depth is governed by the drift controller (see `drift`), which
//! resamples the buffer toward its setpoint. The target here is the prefill
//! threshold and the level trimmed to when playback (re)starts.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// What the playback side gets when it asks for the next frame.
#[derive(Debug, PartialEq)]
pub enum Pop {
    /// The packet for this slot.
    Packet(Vec<u8>),
    /// This slot never arrived, but the following packet is held. Decode that
    /// one with FEC to reconstruct this frame; it stays queued for its own turn.
    LostRecoverable(Vec<u8>),
    /// Nothing to rebuild from. The caller conceals.
    Lost,
    /// Still prefilling, or the stream stopped. Not an error.
    Filling,
}

#[derive(Debug)]
pub struct JitterBuffer {
    slots: BTreeMap<u64, Vec<u8>>,
    target_frames: usize,
    /// Hard cap; beyond this the oldest frames go, to bound latency.
    max_frames: usize,
    /// Next sequence number to hand out. `None` while prefilling.
    next_out: Option<u64>,

    pub pushed: u64,
    pub popped: u64,
    pub lost: u64,
    pub recovered: u64,
    pub late: u64,
    pub dropped_overflow: u64,
    /// Frames discarded at prefill because the cushion overshot the target.
    pub trimmed: u64,
    /// Times playback restarted from empty.
    pub stalls: u64,
}

impl JitterBuffer {
    pub fn new(target_frames: usize, max_frames: usize) -> Self {
        assert!(target_frames >= 1, "target must hold at least one frame");
        assert!(
            max_frames >= target_frames,
            "cap must not be below the target"
        );
        Self {
            slots: BTreeMap::new(),
            target_frames,
            max_frames,
            next_out: None,
            pushed: 0,
            popped: 0,
            lost: 0,
            recovered: 0,
            late: 0,
            dropped_overflow: 0,
            trimmed: 0,
            stalls: 0,
        }
    }

    pub fn depth(&self) -> usize {
        self.slots.len()
    }

    pub fn target_frames(&self) -> usize {
        self.target_frames
    }

    /// Move the prefill threshold. Existing depth is left alone: the drift
    /// controller walks it to the new level by resampling, which is inaudible,
    /// where dropping or inserting a frame here would click.
    pub fn set_target_frames(&mut self, frames: usize) {
        self.target_frames = frames.clamp(1, self.max_frames);
    }

    pub fn playing(&self) -> bool {
        self.next_out.is_some()
    }

    pub fn push(&mut self, ext_seq: u64, packet: Vec<u8>) {
        self.pushed += 1;

        // A packet whose slot has already been handed out is useless.
        if let Some(next) = self.next_out {
            if ext_seq < next {
                self.late += 1;
                return;
            }
        }

        self.slots.insert(ext_seq, packet);

        while self.slots.len() > self.max_frames {
            let Some(&oldest) = self.slots.keys().next() else {
                break;
            };
            self.slots.remove(&oldest);
            self.dropped_overflow += 1;
            if let Some(next) = self.next_out {
                if oldest >= next {
                    self.next_out = Some(oldest + 1);
                }
            }
        }
    }

    /// Take the next packet in sequence.
    pub fn pop(&mut self) -> Pop {
        let next = match self.next_out {
            Some(n) => n,
            None => {
                if self.slots.len() < self.target_frames {
                    return Pop::Filling;
                }
                // Whatever piled up while the sound card was starting is pure
                // latency: production and consumption run at the same rate, so
                // an excess cushion never drains on its own. Shed it before
                // anything is audible — mid-stream this would click.
                while self.slots.len() > self.target_frames {
                    let oldest = *self.slots.keys().next().expect("len > target >= 1");
                    self.slots.remove(&oldest);
                    self.trimmed += 1;
                }
                let first = *self.slots.keys().next().expect("target >= 1");
                self.next_out = Some(first);
                first
            }
        };

        if let Some(packet) = self.slots.remove(&next) {
            self.next_out = Some(next + 1);
            self.popped += 1;
            return Pop::Packet(packet);
        }

        if self.slots.is_empty() {
            // The stream stopped or stalled; rebuild the cushion from scratch.
            self.next_out = None;
            self.stalls += 1;
            return Pop::Filling;
        }

        self.next_out = Some(next + 1);
        self.lost += 1;

        // Opus carries a reduced copy of frame N inside frame N+1 only, so
        // recovery is possible exactly when the immediate successor is here.
        match self.slots.get(&(next + 1)) {
            Some(successor) => {
                self.recovered += 1;
                Pop::LostRecoverable(successor.clone())
            }
            None => Pop::Lost,
        }
    }

    /// Zrzuca nadmiar ponad cel, przeskakując do najświeższych ramek.
    ///
    /// Do wywołania raz, gdy potok jest już napełniony. Pierścień przed kartą
    /// trzyma wtedy ciągły dźwięk, więc przeskok w strumieniu jest sklejką,
    /// a nie dziurą — inaczej niż w środku sesji, gdzie by trzasnęło.
    ///
    /// Prefill przycina poduszkę wcześniej, ale między nim a napełnieniem
    /// pierścienia zdąży napłynąć tyle pakietów, ile trwa rozruch karty; bez
    /// tego drugiego przycięcia regulator dryfu ściągałby ten nadmiar
    /// kilkadziesiąt sekund.
    pub fn trim_to_target(&mut self) -> usize {
        let mut dropped = 0;
        while self.slots.len() > self.target_frames {
            let oldest = *self.slots.keys().next().expect("len > target >= 1");
            self.slots.remove(&oldest);
            self.trimmed += 1;
            dropped += 1;
        }
        if let Some(&first) = self.slots.keys().next() {
            self.next_out = Some(first);
        }
        dropped
    }

    /// Holes as a percentage of frames that should have played. Frames rebuilt
    /// by FEC count too — they were lost on the wire, the listener just did not
    /// hear it, and the encoder still needs to know.
    pub fn loss_pct(&self) -> f32 {
        let expected = self.popped + self.lost;
        if expected == 0 {
            0.0
        } else {
            self.lost as f32 * 100.0 / expected as f32
        }
    }
}

/// Chooses how much cushion the link currently deserves.
///
/// The cushion buys exactly one thing: time for a packet that took the scenic
/// route to still arrive before its slot. So *lateness* is what grows it —
/// a packet that showed up after we had given up on it, or an underrun.
///
/// A packet that was dropped on the wire is not evidence of anything. It is
/// never going to arrive, no matter how long we wait; FEC and concealment deal
/// with it. Growing the cushion on plain loss buys nothing and costs latency
/// permanently, which on a 5% link walks the buffer straight to its ceiling.
///
/// Growth is immediate but rate-limited, so one bad burst cannot slam the
/// target to the maximum. Shrinking waits for a long clean spell, because
/// being wrong in that direction is audible.
#[derive(Debug)]
pub struct AdaptiveTarget {
    current: usize,
    min: usize,
    max: usize,
    clean_since: Instant,
    settle: Duration,
    last_grow: Option<Instant>,
    grow_cooldown: Duration,
}

impl AdaptiveTarget {
    pub fn new(start_frames: usize, min_frames: usize, max_frames: usize) -> Self {
        Self {
            current: start_frames.clamp(min_frames, max_frames),
            min: min_frames,
            max: max_frames,
            clean_since: Instant::now(),
            settle: Duration::from_secs(30),
            last_grow: None,
            grow_cooldown: Duration::from_millis(500),
        }
    }

    /// Shorten the clean-spell requirement. Only tests should need this.
    pub fn with_settle(mut self, settle: Duration) -> Self {
        self.settle = settle;
        self
    }

    pub fn frames(&self) -> usize {
        self.current
    }

    /// A packet arrived too late to be played, or the buffer ran dry: the
    /// cushion was not enough. Do **not** call this for a packet that was
    /// simply lost — see the type-level note.
    pub fn on_late(&mut self, now: Instant) {
        // Any trouble at all postpones shrinking, even when the rate limit
        // stops us from growing again just yet.
        self.clean_since = now;

        if let Some(last) = self.last_grow {
            if now.duration_since(last) < self.grow_cooldown {
                return;
            }
        }
        self.current = (self.current + 2).min(self.max);
        self.last_grow = Some(now);
    }

    /// Call periodically. Returns true when the target changed.
    pub fn tick(&mut self, now: Instant) -> bool {
        if self.current > self.min && now.duration_since(self.clean_since) >= self.settle {
            self.current -= 1;
            self.clean_since = now;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkt(v: u8) -> Vec<u8> {
        vec![v; 4]
    }

    #[test]
    fn holds_back_until_target_is_reached() {
        let mut jb = JitterBuffer::new(3, 10);
        jb.push(0, pkt(0));
        assert_eq!(jb.pop(), Pop::Filling);
        jb.push(1, pkt(1));
        assert_eq!(jb.pop(), Pop::Filling);
        jb.push(2, pkt(2));
        assert_eq!(jb.pop(), Pop::Packet(pkt(0)));
        assert_eq!(jb.pop(), Pop::Packet(pkt(1)));
    }

    #[test]
    fn reorders_within_the_cushion() {
        let mut jb = JitterBuffer::new(3, 10);
        jb.push(0, pkt(0));
        jb.push(2, pkt(2));
        jb.push(1, pkt(1));
        assert_eq!(jb.pop(), Pop::Packet(pkt(0)));
        assert_eq!(jb.pop(), Pop::Packet(pkt(1)));
        assert_eq!(jb.pop(), Pop::Packet(pkt(2)));
        assert_eq!(jb.lost, 0);
    }

    #[test]
    fn a_hole_with_its_successor_present_is_recoverable() {
        let mut jb = JitterBuffer::new(2, 10);
        jb.push(0, pkt(0));
        jb.push(2, pkt(2));
        assert_eq!(jb.pop(), Pop::Packet(pkt(0)));
        // Frame 1 is missing but frame 2 is here: FEC can rebuild it.
        assert_eq!(jb.pop(), Pop::LostRecoverable(pkt(2)));
        // The successor is still delivered for its own slot.
        assert_eq!(jb.pop(), Pop::Packet(pkt(2)));
        assert_eq!(jb.lost, 1);
        assert_eq!(jb.recovered, 1);
    }

    #[test]
    fn a_hole_without_a_successor_falls_back_to_concealment() {
        let mut jb = JitterBuffer::new(2, 10);
        jb.push(0, pkt(0));
        jb.push(3, pkt(3));
        assert_eq!(jb.pop(), Pop::Packet(pkt(0)));
        assert_eq!(
            jb.pop(),
            Pop::Lost,
            "ramki 2 też brakuje, nie ma z czego odtworzyć ramki 1"
        );
        assert_eq!(jb.pop(), Pop::LostRecoverable(pkt(3)));
        assert_eq!(jb.recovered, 1);
    }

    #[test]
    fn a_packet_that_arrives_after_its_slot_is_counted_late() {
        let mut jb = JitterBuffer::new(2, 10);
        jb.push(0, pkt(0));
        jb.push(2, pkt(2));
        jb.pop();
        jb.pop();
        jb.push(1, pkt(1));
        assert_eq!(jb.late, 1);
    }

    #[test]
    fn caps_latency_by_dropping_the_oldest() {
        let mut jb = JitterBuffer::new(2, 4);
        for i in 0..8 {
            jb.push(i, pkt(i as u8));
        }
        assert_eq!(jb.depth(), 4);
        assert_eq!(jb.dropped_overflow, 4);
        assert_eq!(jb.pop(), Pop::Packet(pkt(6)));
        assert_eq!(jb.trimmed, 2);
    }

    #[test]
    fn prefill_overshoot_is_shed_before_the_first_sample() {
        let mut jb = JitterBuffer::new(3, 40);
        for i in 0..10 {
            jb.push(i, pkt(i as u8));
        }
        assert_eq!(jb.pop(), Pop::Packet(pkt(7)));
        assert_eq!(jb.trimmed, 7);
        assert_eq!(jb.depth(), 2);
        assert_eq!(jb.lost, 0, "przycięcie to nie strata");
    }

    #[test]
    fn trimming_after_priming_skips_ahead_without_counting_losses() {
        let mut jb = JitterBuffer::new(3, 40);
        for i in 0..4 {
            jb.push(i, pkt(i as u8));
        }
        assert_eq!(jb.pop(), Pop::Packet(pkt(1)), "prefill przycina do celu");

        // Rozruch karty: zanim potok się napełnił, dopłynęło jeszcze dziesięć.
        for i in 4..14 {
            jb.push(i, pkt(i as u8));
        }
        assert_eq!(jb.trim_to_target(), 9);
        assert_eq!(jb.depth(), 3);
        assert_eq!(
            jb.pop(),
            Pop::Packet(pkt(11)),
            "gramy najświeższe, nie stare"
        );
        assert_eq!(jb.lost, 0, "przeskok to nie strata");
    }

    #[test]
    fn trimming_a_buffer_at_target_does_nothing() {
        let mut jb = JitterBuffer::new(3, 40);
        for i in 0..3 {
            jb.push(i, pkt(i as u8));
        }
        jb.pop();
        let before = jb.depth();
        assert_eq!(jb.trim_to_target(), 0);
        assert_eq!(jb.depth(), before);
    }

    #[test]
    fn recovers_after_the_stream_stalls() {
        let mut jb = JitterBuffer::new(2, 10);
        jb.push(0, pkt(0));
        jb.push(1, pkt(1));
        jb.pop();
        jb.pop();
        assert_eq!(jb.pop(), Pop::Filling);
        assert_eq!(jb.stalls, 1);
        assert!(!jb.playing());
        jb.push(2, pkt(2));
        jb.push(3, pkt(3));
        assert_eq!(jb.pop(), Pop::Packet(pkt(2)));
    }

    #[test]
    fn moving_the_target_does_not_disturb_what_is_queued() {
        let mut jb = JitterBuffer::new(3, 20);
        for i in 0..6 {
            jb.push(i, pkt(i as u8));
        }
        jb.pop(); // trims to the target, then plays
        let before = jb.depth();
        jb.set_target_frames(2);
        assert_eq!(jb.depth(), before, "zmiana celu nic nie wyrzuca");
    }

    #[test]
    fn adaptive_target_grows_at_once_and_shrinks_slowly() {
        let t0 = Instant::now();
        let mut a = AdaptiveTarget::new(3, 2, 12).with_settle(Duration::from_secs(10));

        assert!(!a.tick(t0), "świeży licznik nie skraca od razu");
        a.on_late(t0);
        assert_eq!(a.frames(), 5, "spóźniony pakiet podnosi natychmiast");

        let mut now = t0;
        for expected in [4, 3, 2] {
            now += Duration::from_secs(10);
            assert!(a.tick(now));
            assert_eq!(a.frames(), expected);
        }
        now += Duration::from_secs(60);
        assert!(!a.tick(now), "nie schodzimy poniżej minimum");
        assert_eq!(a.frames(), 2);
    }

    #[test]
    fn a_burst_of_lateness_cannot_slam_the_target_to_the_ceiling() {
        let t0 = Instant::now();
        let mut a = AdaptiveTarget::new(3, 2, 20);
        // Fifty late packets inside one millisecond is one event, not fifty.
        for i in 0..50 {
            a.on_late(t0 + Duration::from_micros(i * 20));
        }
        assert_eq!(a.frames(), 5, "jeden zryw ma podnieść raz");

        // Trouble that keeps coming does keep raising it, just not per packet.
        a.on_late(t0 + Duration::from_millis(600));
        assert_eq!(a.frames(), 7);
    }

    #[test]
    fn repeated_trouble_holds_the_cushion_even_between_growths() {
        let t0 = Instant::now();
        let mut a = AdaptiveTarget::new(3, 2, 20).with_settle(Duration::from_secs(1));
        a.on_late(t0);
        // Inside the growth cooldown, but the clean spell still restarts.
        a.on_late(t0 + Duration::from_millis(100));
        assert!(
            !a.tick(t0 + Duration::from_millis(1_050)),
            "sekunda liczy się od ostatniego kłopotu, nie od ostatniego wzrostu"
        );
    }

    #[test]
    fn adaptive_target_respects_its_ceiling() {
        let mut t = Instant::now();
        let mut a = AdaptiveTarget::new(3, 2, 6);
        for _ in 0..10 {
            a.on_late(t);
            t += Duration::from_secs(1);
        }
        assert_eq!(a.frames(), 6);
    }
}
