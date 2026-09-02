//! Sample-rate conversion with a ratio that can move while it runs.
//!
//! Two jobs, one implementation:
//!   * a fixed ratio, when a device insists on 44.1 kHz and the engine wants 48
//!   * a ratio nudged by a fraction of a percent, to cancel clock drift (§drift)
//!
//! Both live off the audio thread — the pacer resamples, the callback only
//! drains a ring buffer.

use anyhow::{Context, Result};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

/// How far the ratio may stray from its base. Drift needs 0.5%; the margin
/// costs only a slightly larger internal buffer.
const MAX_RELATIVE: f64 = 1.05;

pub struct VariableResampler {
    inner: SincFixedIn<f32>,
    /// Nominal conversion, e.g. 48000/44100. Corrections multiply this.
    base_ratio: f64,
    applied_correction: f64,
    input: Vec<Vec<f32>>,
    output: Vec<Vec<f32>>,
    chunk: usize,
}

impl VariableResampler {
    /// `base_ratio` is output rate over input rate; `chunk` is how many input
    /// samples every call will supply.
    pub fn new(base_ratio: f64, chunk: usize) -> Result<Self> {
        let params = SincInterpolationParameters {
            sinc_len: 128,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 128,
            window: WindowFunction::BlackmanHarris2,
        };
        let inner = SincFixedIn::<f32>::new(base_ratio, MAX_RELATIVE, params, chunk, 1)
            .context("nie mogę utworzyć resamplera")?;
        let out_max = inner.output_frames_max();
        Ok(Self {
            inner,
            base_ratio,
            applied_correction: 0.0,
            input: vec![vec![0f32; chunk]],
            output: vec![vec![0f32; out_max]],
            chunk,
        })
    }

    pub fn chunk(&self) -> usize {
        self.chunk
    }

    pub fn base_ratio(&self) -> f64 {
        self.base_ratio
    }

    /// Apply a drift correction. Ramping spreads the change across the next
    /// chunk instead of stepping the ratio, which would click.
    pub fn set_correction(&mut self, correction: f64) -> Result<()> {
        // Re-programming the sinc tables on every 10 ms chunk is wasted work
        // for a change far below what anyone can hear.
        if (correction - self.applied_correction).abs() < 1e-7 {
            return Ok(());
        }
        self.inner
            .set_resample_ratio(self.base_ratio * (1.0 + correction), true)
            .context("nie mogę zmienić współczynnika resamplera")?;
        self.applied_correction = correction;
        Ok(())
    }

    pub fn correction(&self) -> f64 {
        self.applied_correction
    }

    /// Convert exactly `chunk` input samples, appending the result to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<()> {
        anyhow::ensure!(
            input.len() == self.chunk,
            "resampler oczekuje {} próbek, dostał {}",
            self.chunk,
            input.len()
        );
        self.input[0].copy_from_slice(input);

        let needed = self.inner.output_frames_next();
        if self.output[0].len() < needed {
            self.output[0].resize(needed, 0.0);
        }

        let (_used, written) = self
            .inner
            .process_into_buffer(&self.input, &mut self.output, None)
            .context("resampling")?;
        out.extend_from_slice(&self.output[0][..written]);
        Ok(())
    }
}

/// True when no conversion is needed at all, so callers can skip the resampler
/// entirely on the common 48 kHz path.
pub fn is_identity(from_rate: u32, to_rate: u32) -> bool {
    from_rate == to_rate
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(n: usize, hz: f32, rate: f32) -> Vec<f32> {
        (0..n)
            .map(|i| (std::f32::consts::TAU * hz * i as f32 / rate).sin())
            .collect()
    }

    #[test]
    fn converts_44100_to_48000_at_the_right_length() {
        let chunk = 441;
        let mut r = VariableResampler::new(48_000.0 / 44_100.0, chunk).unwrap();
        let input = sine(chunk * 20, 440.0, 44_100.0);
        let mut out = Vec::new();
        for c in input.chunks_exact(chunk) {
            r.process(c, &mut out).unwrap();
        }
        let expected = input.len() as f64 * 48_000.0 / 44_100.0;
        let error = (out.len() as f64 - expected).abs() / expected;
        assert!(error < 0.01, "długość odbiega o {:.2}%", error * 100.0);
    }

    #[test]
    fn identity_ratio_preserves_length_and_signal() {
        let chunk = 480;
        let mut r = VariableResampler::new(1.0, chunk).unwrap();
        let input = sine(chunk * 30, 440.0, 48_000.0);
        let mut out = Vec::new();
        for c in input.chunks_exact(chunk) {
            r.process(c, &mut out).unwrap();
        }
        // The sinc filter holds part of a chunk internally, so the totals
        // differ by its length, not by a drifting amount.
        let slack = (out.len() as i64 - input.len() as i64).abs();
        assert!(slack <= 256, "różnica długości {slack} próbek");

        // That same latency rules out a sample-by-sample comparison, so judge
        // the settled tail by energy.
        let tail = &out[out.len() - 4800..];
        let rms = (tail.iter().map(|s| (s * s) as f64).sum::<f64>() / tail.len() as f64).sqrt();
        assert!((rms - 0.707).abs() < 0.05, "RMS sinusa: {rms:.3}");
    }

    #[test]
    fn a_correction_changes_the_output_length_in_the_right_direction() {
        let chunk = 480;
        let mut r = VariableResampler::new(1.0, chunk).unwrap();
        let input = sine(chunk, 440.0, 48_000.0);

        let mut baseline = Vec::new();
        for _ in 0..100 {
            r.process(&input, &mut baseline).unwrap();
        }

        // A negative correction emits fewer samples per input sample, which is
        // how the receiver drains an over-full buffer.
        r.set_correction(-0.005).unwrap();
        let mut drained = Vec::new();
        for _ in 0..100 {
            r.process(&input, &mut drained).unwrap();
        }
        assert!(
            drained.len() < baseline.len(),
            "ujemna korekta ma skracać wyjście: {} vs {}",
            drained.len(),
            baseline.len()
        );

        r.set_correction(0.005).unwrap();
        let mut stretched = Vec::new();
        for _ in 0..100 {
            r.process(&input, &mut stretched).unwrap();
        }
        assert!(stretched.len() > baseline.len());
    }

    #[test]
    fn rejects_a_chunk_of_the_wrong_size() {
        let mut r = VariableResampler::new(1.0, 480).unwrap();
        let mut out = Vec::new();
        assert!(r.process(&[0.0; 100], &mut out).is_err());
    }
}
