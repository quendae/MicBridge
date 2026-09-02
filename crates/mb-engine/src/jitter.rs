//! Reordering buffer that absorbs network jitter.
//!
//! Frames arrive keyed by an extended (wrap-free) sequence number and leave in
//! order at a steady rate. The buffer holds back `target_frames` before it
//! starts playing, so a packet that arrives late still has room to slot in.
//!
//! Milestone 1 keeps the target fixed. The adaptive target and the clock-drift
//! controller land in milestone 2 and plug in here without changing the API.

use std::collections::BTreeMap;

/// What the playback side gets when it asks for the next frame.
#[derive(Debug, PartialEq)]
pub enum Pop {
    /// A frame in correct order.
    Frame(Vec<i16>),
    /// The frame at this position never arrived. Milestone 2 replaces this with
    /// Opus packet-loss concealment; for now the caller plays silence.
    Lost,
    /// Still prefilling, or the stream stopped. Not an error.
    Filling,
}

#[derive(Debug)]
pub struct JitterBuffer {
    slots: BTreeMap<u64, Vec<i16>>,
    /// Frames held before playback starts.
    target_frames: usize,
    /// Hard cap; beyond this the oldest frames are dropped to bound latency.
    max_frames: usize,
    /// Next sequence number to hand out. `None` while prefilling.
    next_out: Option<u64>,

    pub pushed: u64,
    pub popped: u64,
    pub lost: u64,
    pub late: u64,
    pub dropped_overflow: u64,
    /// Frames discarded at prefill because the cushion overshot the target.
    pub trimmed: u64,
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
            late: 0,
            dropped_overflow: 0,
            trimmed: 0,
        }
    }

    /// Frames currently held.
    pub fn depth(&self) -> usize {
        self.slots.len()
    }

    pub fn target_frames(&self) -> usize {
        self.target_frames
    }

    /// True once enough frames have accumulated for playback to run.
    pub fn playing(&self) -> bool {
        self.next_out.is_some()
    }

    pub fn push(&mut self, ext_seq: u64, frame: Vec<i16>) {
        self.pushed += 1;

        // A frame whose slot has already been handed out is useless.
        if let Some(next) = self.next_out {
            if ext_seq < next {
                self.late += 1;
                return;
            }
        }

        self.slots.insert(ext_seq, frame);

        // Bound the buffer: if the sender is ahead of us, drop the oldest
        // frames rather than let latency grow without limit.
        while self.slots.len() > self.max_frames {
            if let Some(&oldest) = self.slots.keys().next() {
                self.slots.remove(&oldest);
                self.dropped_overflow += 1;
                if let Some(next) = self.next_out {
                    if oldest >= next {
                        self.next_out = Some(oldest + 1);
                    }
                }
            }
        }
    }

    /// Take the next frame in sequence.
    pub fn pop(&mut self) -> Pop {
        let next = match self.next_out {
            Some(n) => n,
            None => {
                if self.slots.len() < self.target_frames {
                    return Pop::Filling;
                }
                // Whatever piled up while the sound card was starting is pure
                // latency: production and consumption run at the same rate, so
                // an excess cushion never drains on its own. Shed it here,
                // before anything is audible — dropping frames mid-stream would
                // click, dropping them before the first sample costs nothing.
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

        match self.slots.remove(&next) {
            Some(frame) => {
                self.next_out = Some(next + 1);
                self.popped += 1;
                Pop::Frame(frame)
            }
            None if self.slots.is_empty() => {
                // Nothing left at all: the stream stopped or stalled. Go back to
                // prefilling so the next burst rebuilds the cushion.
                self.next_out = None;
                Pop::Filling
            }
            None => {
                self.next_out = Some(next + 1);
                self.lost += 1;
                Pop::Lost
            }
        }
    }

    /// Loss as a fraction of frames that should have played, in percent.
    pub fn loss_pct(&self) -> f32 {
        let expected = self.popped + self.lost;
        if expected == 0 {
            0.0
        } else {
            self.lost as f32 * 100.0 / expected as f32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(v: i16) -> Vec<i16> {
        vec![v; 4]
    }

    #[test]
    fn holds_back_until_target_is_reached() {
        let mut jb = JitterBuffer::new(3, 10);
        jb.push(0, frame(0));
        assert_eq!(jb.pop(), Pop::Filling);
        jb.push(1, frame(1));
        assert_eq!(jb.pop(), Pop::Filling);
        jb.push(2, frame(2));
        assert_eq!(jb.pop(), Pop::Frame(frame(0)));
        assert_eq!(jb.pop(), Pop::Frame(frame(1)));
    }

    #[test]
    fn reorders_within_the_cushion() {
        let mut jb = JitterBuffer::new(3, 10);
        jb.push(0, frame(0));
        jb.push(2, frame(2));
        jb.push(1, frame(1)); // out of order, but still in time
        assert_eq!(jb.pop(), Pop::Frame(frame(0)));
        assert_eq!(jb.pop(), Pop::Frame(frame(1)));
        assert_eq!(jb.pop(), Pop::Frame(frame(2)));
        assert_eq!(jb.lost, 0);
    }

    #[test]
    fn reports_a_hole_once_and_moves_on() {
        let mut jb = JitterBuffer::new(2, 10);
        jb.push(0, frame(0));
        jb.push(2, frame(2));
        assert_eq!(jb.pop(), Pop::Frame(frame(0)));
        assert_eq!(jb.pop(), Pop::Lost);
        assert_eq!(jb.pop(), Pop::Frame(frame(2)));
        assert_eq!(jb.lost, 1);
    }

    #[test]
    fn a_frame_that_arrives_after_its_slot_is_counted_late_not_played() {
        let mut jb = JitterBuffer::new(2, 10);
        jb.push(0, frame(0));
        jb.push(2, frame(2));
        jb.pop(); // 0
        jb.pop(); // hole at 1
        jb.push(1, frame(1)); // too late now
        assert_eq!(jb.late, 1);
        assert_eq!(jb.pop(), Pop::Frame(frame(2)));
    }

    #[test]
    fn caps_latency_by_dropping_the_oldest() {
        let mut jb = JitterBuffer::new(2, 4);
        for i in 0..8 {
            jb.push(i, frame(i as i16));
        }
        assert_eq!(jb.depth(), 4);
        assert_eq!(jb.dropped_overflow, 4);
        // The cap left four frames; prefill then trims down to the target of
        // two, so playback starts at the second-freshest frame.
        assert_eq!(jb.pop(), Pop::Frame(frame(6)));
        assert_eq!(jb.trimmed, 2);
    }

    #[test]
    fn prefill_overshoot_is_shed_before_the_first_sample() {
        let mut jb = JitterBuffer::new(3, 40);
        // A slow-starting sound card lets ten frames pile up.
        for i in 0..10 {
            jb.push(i, frame(i as i16));
        }
        // Playback begins at the freshest cushion, not the stalest frame.
        assert_eq!(jb.pop(), Pop::Frame(frame(7)));
        assert_eq!(jb.trimmed, 7);
        assert_eq!(jb.depth(), 2, "cel minus wydana ramka");
        assert_eq!(jb.pop(), Pop::Frame(frame(8)));
        assert_eq!(jb.pop(), Pop::Frame(frame(9)));
        assert_eq!(jb.lost, 0, "przycięcie to nie strata");
    }

    #[test]
    fn trimming_happens_again_after_a_stall() {
        let mut jb = JitterBuffer::new(2, 40);
        jb.push(0, frame(0));
        jb.push(1, frame(1));
        jb.pop();
        jb.pop();
        assert_eq!(jb.pop(), Pop::Filling);
        for i in 10..20 {
            jb.push(i, frame(i as i16));
        }
        assert_eq!(jb.pop(), Pop::Frame(frame(18)));
    }

    #[test]
    fn recovers_after_the_stream_stalls() {
        let mut jb = JitterBuffer::new(2, 10);
        jb.push(0, frame(0));
        jb.push(1, frame(1));
        assert_eq!(jb.pop(), Pop::Frame(frame(0)));
        assert_eq!(jb.pop(), Pop::Frame(frame(1)));
        assert_eq!(jb.pop(), Pop::Filling, "empty buffer is not a loss");
        assert!(!jb.playing());
        jb.push(2, frame(2));
        jb.push(3, frame(3));
        assert_eq!(jb.pop(), Pop::Frame(frame(2)));
    }

    #[test]
    fn loss_percentage_counts_holes_against_frames_played() {
        let mut jb = JitterBuffer::new(1, 10);
        jb.push(0, frame(0));
        assert_eq!(jb.pop(), Pop::Frame(frame(0)));
        jb.push(2, frame(2)); // frame 1 never arrives
        assert_eq!(jb.pop(), Pop::Lost);
        assert_eq!(jb.pop(), Pop::Frame(frame(2)));
        // Two frames played, one hole: one in three.
        assert!((jb.loss_pct() - 33.3).abs() < 0.5, "got {}", jb.loss_pct());
    }
}
