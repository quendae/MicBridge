//! Opus, wrapped so the loss-recovery paths are explicit.
//!
//! Three ways a frame reaches the output, in descending order of quality:
//!   `decode`   the packet arrived
//!   `decode_fec` it did not, but the *next* packet carries a low-bitrate copy
//!   `conceal`  neither, so the decoder extrapolates from its own model
//!
//! In-band FEC is why the jitter buffer hands us the following packet instead
//! of just reporting a hole: Opus embeds a reduced copy of frame N inside
//! frame N+1, and recovering from it costs one frame of latency, not a gap.

use anyhow::{Context, Result};
use opus::{Application, Channels, Decoder, Encoder};

use mb_proto::{FRAME_SAMPLES, SAMPLE_RATE};

/// Worst case for a 10 ms Opus frame; real ones sit near 30-60 B.
const MAX_PACKET: usize = 1275;

pub struct OpusEncoder {
    inner: Encoder,
    packet: Vec<u8>,
    expected_loss: u8,
}

impl OpusEncoder {
    /// `bitrate` in bits per second. 24 kbps is transparent for speech at
    /// 48 kHz mono and leaves headroom for FEC to be worth sending.
    pub fn new(bitrate: u32) -> Result<Self> {
        let mut inner = Encoder::new(SAMPLE_RATE, Channels::Mono, Application::Voip)
            .context("nie mogę utworzyć kodera Opus")?;
        inner.set_bitrate(opus::Bitrate::Bits(bitrate as i32))?;
        // FEC is useless unless the encoder believes packets go missing, so a
        // non-zero starting estimate matters; STATS refines it from reality.
        inner.set_inband_fec(true)?;
        inner.set_packet_loss_perc(5)?;
        Ok(Self {
            inner,
            packet: vec![0u8; MAX_PACKET],
            expected_loss: 5,
        })
    }

    /// Feed back what the receiver actually measures. Raising this makes the
    /// encoder spend more bits on the redundant copy; lowering it spends them
    /// on the primary frame instead.
    pub fn set_expected_loss(&mut self, pct: f32) -> Result<()> {
        let clamped = pct.clamp(0.0, 30.0).round() as u8;
        if clamped != self.expected_loss {
            self.inner.set_packet_loss_perc(clamped as i32)?;
            self.expected_loss = clamped;
            tracing::debug!(pct = clamped, "zmieniono zakładaną stratę pakietów");
        }
        Ok(())
    }

    pub fn expected_loss(&self) -> u8 {
        self.expected_loss
    }

    pub fn encode(&mut self, pcm: &[i16]) -> Result<&[u8]> {
        let n = self
            .inner
            .encode(pcm, &mut self.packet)
            .context("kodowanie Opus")?;
        Ok(&self.packet[..n])
    }
}

pub struct OpusDecoder {
    inner: Decoder,
}

impl OpusDecoder {
    pub fn new() -> Result<Self> {
        Ok(Self {
            inner: Decoder::new(SAMPLE_RATE, Channels::Mono)
                .context("nie mogę utworzyć dekodera Opus")?,
        })
    }

    /// Normal path: this packet, decoded into `out`. Returns samples written.
    pub fn decode(&mut self, packet: &[u8], out: &mut [i16]) -> Result<usize> {
        Ok(self.inner.decode(packet, out, false)?)
    }

    /// The frame before `next_packet` was lost; reconstruct it from the
    /// redundant copy `next_packet` carries.
    ///
    /// If the encoder chose not to include FEC data, libopus falls back to
    /// concealment internally, so this never fails for lack of redundancy.
    pub fn decode_fec(&mut self, next_packet: &[u8], out: &mut [i16]) -> Result<usize> {
        Ok(self.inner.decode(next_packet, out, true)?)
    }

    /// Nothing arrived and nothing follows: extrapolate.
    pub fn conceal(&mut self, out: &mut [i16]) -> Result<usize> {
        Ok(self.inner.decode(&[], out, false)?)
    }
}

/// Scratch sized for one decoded frame.
pub fn frame_buffer() -> Vec<i16> {
    vec![0i16; FRAME_SAMPLES]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(frames: usize) -> Vec<Vec<i16>> {
        let mut phase = 0f32;
        let step = std::f32::consts::TAU * 440.0 / SAMPLE_RATE as f32;
        (0..frames)
            .map(|_| {
                (0..FRAME_SAMPLES)
                    .map(|_| {
                        let v = (phase.sin() * 8000.0) as i16;
                        phase += step;
                        v
                    })
                    .collect()
            })
            .collect()
    }

    fn energy(samples: &[i16]) -> f64 {
        samples.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / samples.len() as f64
    }

    #[test]
    fn a_frame_survives_the_round_trip() {
        let mut enc = OpusEncoder::new(24_000).unwrap();
        let mut dec = OpusDecoder::new().unwrap();
        let frames = sine(20);
        let mut out = frame_buffer();

        // Opus needs a few frames to settle; judge the later ones.
        let mut last = 0.0;
        for f in &frames {
            let packet = enc.encode(f).unwrap();
            assert!(!packet.is_empty(), "koder nie może zwrócić pustej ramki");
            assert!(packet.len() < 200, "10 ms przy 24 kbps to dziesiątki bajtów");
            let n = dec.decode(packet, &mut out).unwrap();
            assert_eq!(n, FRAME_SAMPLES);
            last = energy(&out);
        }
        let want = energy(&frames[frames.len() - 1]);
        let ratio = last / want;
        assert!(
            (0.5..2.0).contains(&ratio),
            "energia po dekodowaniu odbiega za bardzo: {ratio:.2}"
        );
    }

    #[test]
    fn concealment_produces_a_full_frame_not_a_gap() {
        let mut enc = OpusEncoder::new(24_000).unwrap();
        let mut dec = OpusDecoder::new().unwrap();
        for f in sine(10) {
            let p = enc.encode(&f).unwrap();
            let mut out = frame_buffer();
            dec.decode(p, &mut out).unwrap();
        }
        let mut out = frame_buffer();
        let n = dec.conceal(&mut out).unwrap();
        assert_eq!(n, FRAME_SAMPLES);
        assert!(
            energy(&out) > 1.0,
            "PLC ma kontynuować sygnał, nie wstawiać ciszę"
        );
    }

    #[test]
    fn fec_recovers_a_dropped_frame_from_its_successor() {
        let mut enc = OpusEncoder::new(24_000).unwrap();
        enc.set_expected_loss(10.0).unwrap();
        let mut dec = OpusDecoder::new().unwrap();

        let frames = sine(30);
        let packets: Vec<Vec<u8>> = frames
            .iter()
            .map(|f| enc.encode(f).unwrap().to_vec())
            .collect();

        // Prime the decoder, then drop frame 20 and rebuild it from frame 21.
        for p in &packets[..20] {
            let mut out = frame_buffer();
            dec.decode(p, &mut out).unwrap();
        }
        let mut recovered = frame_buffer();
        let n = dec.decode_fec(&packets[21], &mut recovered).unwrap();
        assert_eq!(n, FRAME_SAMPLES);
        assert!(
            energy(&recovered) > 1.0,
            "odtworzona ramka nie może być ciszą"
        );

        // The successor still decodes normally afterwards.
        let mut out = frame_buffer();
        assert_eq!(dec.decode(&packets[21], &mut out).unwrap(), FRAME_SAMPLES);
    }

    #[test]
    fn expected_loss_is_clamped_and_only_pushed_on_change() {
        let mut enc = OpusEncoder::new(24_000).unwrap();
        enc.set_expected_loss(-5.0).unwrap();
        assert_eq!(enc.expected_loss(), 0);
        enc.set_expected_loss(90.0).unwrap();
        assert_eq!(enc.expected_loss(), 30);
    }
}
