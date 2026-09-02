//! Receiver-side measurements reported back to the sender once a second.

use std::time::Instant;

/// Interarrival jitter, smoothed exactly as RFC 3550 §6.4.1 defines it, so the
/// number is comparable with what any RTP tool reports for the same stream.
#[derive(Debug)]
pub struct StreamStats {
    jitter: f64,
    last_transit: Option<f64>,
    last_arrival: Option<Instant>,
    frame_ms: f64,
}

impl StreamStats {
    pub fn new(frame_ms: u32) -> Self {
        Self {
            jitter: 0.0,
            last_transit: None,
            last_arrival: None,
            frame_ms: frame_ms as f64,
        }
    }

    /// Feed one arrival. `ext_seq` is the wrap-free sequence number.
    ///
    /// Without a shared clock we cannot measure absolute transit time, so we
    /// use the packet's expected send time (sequence x frame duration) against
    /// local arrival time. The offset cancels out; only its variation matters.
    pub fn on_packet(&mut self, ext_seq: u64, arrival: Instant) {
        let Some(first) = self.last_arrival else {
            self.last_arrival = Some(arrival);
            self.last_transit = Some(-(ext_seq as f64 * self.frame_ms));
            return;
        };

        let elapsed_ms = arrival.duration_since(first).as_secs_f64() * 1000.0;
        let transit = elapsed_ms - ext_seq as f64 * self.frame_ms;

        if let Some(prev) = self.last_transit {
            let d = (transit - prev).abs();
            // Single-pole smoothing with gain 1/16, as in the RFC.
            self.jitter += (d - self.jitter) / 16.0;
        }
        self.last_transit = Some(transit);
    }

    pub fn jitter_ms(&self) -> f32 {
        self.jitter as f32
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_perfectly_paced_stream_has_no_jitter() {
        let mut s = StreamStats::new(10);
        let t0 = Instant::now();
        for i in 0..50u64 {
            s.on_packet(i, t0 + Duration::from_millis(i * 10));
        }
        assert!(s.jitter_ms() < 0.01, "got {}", s.jitter_ms());
    }

    #[test]
    fn wobbling_arrivals_raise_the_estimate() {
        let mut s = StreamStats::new(10);
        let t0 = Instant::now();
        for i in 0..50u64 {
            let wobble = if i % 2 == 0 { 0 } else { 8 };
            s.on_packet(i, t0 + Duration::from_millis(i * 10 + wobble));
        }
        assert!(s.jitter_ms() > 2.0, "got {}", s.jitter_ms());
    }
}
