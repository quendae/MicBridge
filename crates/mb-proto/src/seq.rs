//! Extension of RTP's 16-bit sequence number to a monotonic 64-bit counter.
//!
//! Two consumers need this: the jitter buffer, which must order packets across
//! the wrap at 65535, and the AEAD nonce, which must never repeat a
//! (rollover counter, sequence) pair for a given key.

/// Half the sequence space. A gap larger than this is read as a wrap rather
/// than a jump, which is the standard heuristic (RFC 3711 §3.3.1).
const HALF: i64 = 1 << 15;

#[derive(Debug, Default, Clone)]
pub struct SeqExtender {
    /// Rollover counter: how many times the 16-bit sequence has wrapped.
    roc: u32,
    highest: Option<u16>,
}

impl SeqExtender {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current rollover counter — used as part of the AEAD nonce.
    pub fn roc(&self) -> u32 {
        self.roc
    }

    /// Map a wire sequence number onto a monotonic counter.
    ///
    /// Packets that arrive slightly out of order map back to their true index,
    /// including across a wrap, so the jitter buffer can reorder them.
    pub fn extend(&mut self, seq: u16) -> u64 {
        let (ext, roc, highest) = self.resolve(seq);
        self.roc = roc;
        self.highest = Some(highest);
        ext
    }

    /// The same mapping without committing to it.
    ///
    /// A forged packet must not be able to move the rollover counter: the
    /// receiver decrypts against the peeked value and only calls `extend` once
    /// the packet has proven it is ours.
    pub fn peek(&self, seq: u16) -> u64 {
        self.resolve(seq).0
    }

    /// Returns the extended sequence plus the state it would leave behind.
    fn resolve(&self, seq: u16) -> (u64, u32, u16) {
        let Some(highest) = self.highest else {
            return (self.roc as u64 * 65_536 + seq as u64, self.roc, seq);
        };

        let delta = seq as i64 - highest as i64;
        let (roc, next_highest, next_roc) = if delta < -HALF {
            // Forward across the wrap: 65530 -> 3.
            let bumped = self.roc.wrapping_add(1);
            (bumped, seq, bumped)
        } else if delta > HALF {
            // A late packet from before the wrap: 3 -> 65530.
            (self.roc.wrapping_sub(1), highest, self.roc)
        } else if delta > 0 {
            (self.roc, seq, self.roc)
        } else {
            (self.roc, highest, self.roc)
        };

        (roc as u64 * 65_536 + seq as u64, next_roc, next_highest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_up_in_order() {
        let mut e = SeqExtender::new();
        assert_eq!(e.extend(0), 0);
        assert_eq!(e.extend(1), 1);
        assert_eq!(e.extend(2), 2);
    }

    #[test]
    fn survives_the_wrap() {
        let mut e = SeqExtender::new();
        assert_eq!(e.extend(65_534), 65_534);
        assert_eq!(e.extend(65_535), 65_535);
        assert_eq!(e.extend(0), 65_536);
        assert_eq!(e.extend(1), 65_537);
        assert_eq!(e.roc(), 1);
    }

    #[test]
    fn late_packet_across_the_wrap_stays_behind() {
        let mut e = SeqExtender::new();
        e.extend(65_535);
        e.extend(2);
        // 65534 arrives after the wrap; it belongs to the previous rollover.
        assert_eq!(e.extend(65_534), 65_534);
        assert_eq!(e.roc(), 1, "a late packet must not rewind the counter");
    }

    #[test]
    fn reordering_within_a_window_keeps_true_order() {
        let mut e = SeqExtender::new();
        e.extend(100);
        assert_eq!(e.extend(103), 103);
        assert_eq!(e.extend(101), 101);
        assert_eq!(e.extend(102), 102);
    }

    #[test]
    fn peeking_does_not_move_the_rollover_counter() {
        let mut ext = SeqExtender::new();
        ext.extend(65_530);
        // Podrobiony pakiet zza zawinięcia: gdyby przesunął licznik, kolejne
        // prawdziwe pakiety liczyłyby się od złej wartości.
        assert_eq!(ext.peek(3), ext.peek(3));
        assert_eq!(ext.roc(), 0, "podejrzenie niczego nie zmienia");
        assert_eq!(ext.extend(65_531), 65_531);
        assert_eq!(ext.extend(3), 65_536 + 3, "prawdziwe zawinięcie liczy się");
        assert_eq!(ext.roc(), 1);
    }
}
