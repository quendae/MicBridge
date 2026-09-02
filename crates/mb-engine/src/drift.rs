//! Clock-drift controller.
//!
//! Two machines, two crystals. The sender calls it 48000 Hz, the receiver calls
//! it 48000 Hz, and the real frequencies differ by tens of parts per million.
//! At 30 ppm the buffer gains or loses a second every nine hours — which shows
//! up as either creeping latency or periodic underrun clicks, and is the defect
//! that kills naive implementations after twenty minutes of listening.
//!
//! The fix is not to drop or duplicate samples. It is to resample by a fraction
//! of a percent: a PI controller watches smoothed buffer depth and nudges the
//! resampler ratio within +/-0.5%, which is nine cents of pitch — inaudible on
//! speech, and enough to absorb any crystal you will meet.
//!
//! Sign convention, since it is easy to get backwards: a positive error means
//! the buffer holds more than we want, so we must consume input faster than we
//! emit output, which means a *negative* correction to the resample ratio.

/// Hard limit on the correction. Beyond this the pitch shift starts to be
/// audible, and anything needing more is a real fault, not drift.
pub const MAX_CORRECTION: f64 = 0.005;

/// Smoothing time constant for depth. Long enough to ignore packet-to-packet
/// jitter, short enough to react to a genuine step within a few seconds.
const DEPTH_TAU_S: f32 = 2.0;

/// Correction per millisecond of steady error. 10 ms of error asks for 0.2%.
const KP: f64 = 2.0e-4;
/// Integral gain: removes the residual error a proportional-only loop leaves,
/// which is exactly the constant drift we are here to cancel.
const KI: f64 = 2.0e-5;

#[derive(Debug)]
pub struct DriftController {
    setpoint_ms: f32,
    depth_ema_ms: f32,
    seeded: bool,
    integral: f64,
    correction: f64,
}

impl DriftController {
    pub fn new(setpoint_ms: f32) -> Self {
        Self {
            setpoint_ms,
            depth_ema_ms: setpoint_ms,
            seeded: false,
            integral: 0.0,
            correction: 0.0,
        }
    }

    /// Move the target. The loop walks the buffer there by resampling rather
    /// than by inserting or dropping a frame, so changing it is inaudible.
    pub fn set_setpoint(&mut self, ms: f32) {
        self.setpoint_ms = ms;
    }

    pub fn setpoint_ms(&self) -> f32 {
        self.setpoint_ms
    }

    pub fn depth_ema_ms(&self) -> f32 {
        self.depth_ema_ms
    }

    /// Current multiplicative correction to the resample ratio.
    pub fn correction(&self) -> f64 {
        self.correction
    }

    /// Accumulated error. In steady state this is what holds the correction
    /// that cancels the drift, so watching it is how you tell a settled loop
    /// from one that is still hunting.
    pub fn integral(&self) -> f64 {
        self.integral
    }

    /// Feed one depth observation; returns the correction to apply.
    pub fn update(&mut self, depth_ms: f32, dt_s: f32) -> f64 {
        if !self.seeded {
            // Starting the average at the first real reading avoids a spurious
            // ramp while the filter fills.
            self.depth_ema_ms = depth_ms;
            self.seeded = true;
        } else {
            let alpha = 1.0 - (-dt_s / DEPTH_TAU_S).exp();
            self.depth_ema_ms += alpha * (depth_ms - self.depth_ema_ms);
        }

        let error = (self.depth_ema_ms - self.setpoint_ms) as f64;
        let raw = KP * error + KI * self.integral;

        // Integrate only while the output is not pinned, so a long saturation
        // cannot wind up a correction we would have to unwind afterwards.
        if raw.abs() < MAX_CORRECTION {
            self.integral += error * dt_s as f64;
        }

        self.correction = -(KP * error + KI * self.integral).clamp(-MAX_CORRECTION, MAX_CORRECTION);
        self.correction
    }

    /// After a stall the buffer restarts from prefill; the accumulated integral
    /// describes a world that no longer exists.
    pub fn reset(&mut self) {
        self.integral = 0.0;
        self.correction = 0.0;
        self.seeded = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Closed-loop model of the receiver.
    ///
    /// Input arrives at (1 + drift) times nominal; the resampler consumes it at
    /// (1 - correction) times nominal. Depth is measured in milliseconds of
    /// audio, so it moves at 1000 * (drift + correction) ms per second.
    fn simulate(drift_ppm: f64, start_depth_ms: f32, seconds: f64) -> (f32, f64) {
        let mut ctl = DriftController::new(30.0);
        let drift = drift_ppm * 1e-6;
        let dt = 0.02_f64;
        let mut depth = start_depth_ms as f64;

        for _ in 0..(seconds / dt) as usize {
            let c = ctl.update(depth as f32, dt as f32);
            depth += 1000.0 * (drift + c) * dt;
            depth = depth.max(0.0);
        }
        (depth as f32, ctl.correction())
    }

    #[test]
    fn cancels_a_positive_drift_instead_of_letting_the_buffer_grow() {
        // Without a controller, +30 ppm adds 108 ms of latency per hour.
        let (depth, correction) = simulate(30.0, 30.0, 3600.0);
        assert!(
            (depth - 30.0).abs() < 2.0,
            "bufor odjechał od celu: {depth:.1} ms"
        );
        assert!(
            (correction + 30e-6).abs() < 5e-6,
            "korekta powinna zrównoważyć dryf, jest {correction:.2e}"
        );
    }

    #[test]
    fn cancels_a_negative_drift_without_underrunning() {
        let (depth, correction) = simulate(-30.0, 30.0, 3600.0);
        assert!((depth - 30.0).abs() < 2.0, "bufor: {depth:.1} ms");
        assert!(
            correction > 0.0,
            "przy wolniejszym nadawcy zwalniamy odtwarzanie"
        );
    }

    #[test]
    fn pulls_a_startup_overshoot_back_to_the_setpoint() {
        let (depth, _) = simulate(0.0, 90.0, 600.0);
        assert!((depth - 30.0).abs() < 2.0, "bufor: {depth:.1} ms");
    }

    #[test]
    fn never_shifts_pitch_more_than_half_a_percent() {
        // An absurd error must not produce an audible pitch shift.
        let mut ctl = DriftController::new(30.0);
        for _ in 0..1000 {
            let c = ctl.update(5000.0, 0.02);
            assert!(c.abs() <= MAX_CORRECTION + 1e-12, "korekta {c:.4}");
        }
    }

    #[test]
    fn the_integral_does_not_accumulate_while_the_output_is_pinned() {
        let mut ctl = DriftController::new(30.0);
        // Ten seconds of an error far past anything the correction can fix.
        for _ in 0..500 {
            assert_eq!(ctl.update(5000.0, 0.02), -MAX_CORRECTION);
        }
        // Integrating here would buy nothing — the output is already at its
        // limit — and would have to be unwound afterwards as minutes of
        // wrong-speed playback in the opposite direction.
        assert_eq!(ctl.integral(), 0.0, "całka rozkręciła się mimo nasycenia");
    }

    #[test]
    fn recovers_from_a_pinned_fault_once_the_loop_is_closed_again() {
        let mut ctl = DriftController::new(30.0);
        for _ in 0..500 {
            ctl.update(5000.0, 0.02);
        }
        // Closed loop from the setpoint: the stale average drains the buffer
        // for a moment, then the loop brings it back.
        let mut depth = 30.0_f64;
        for _ in 0..(300.0 / 0.02) as usize {
            let c = ctl.update(depth as f32, 0.02);
            depth = (depth + 1000.0 * c * 0.02).max(0.0);
        }
        assert!((depth - 30.0).abs() < 1.0, "bufor: {depth:.1} ms");
    }

    #[test]
    fn a_moved_setpoint_is_reached() {
        let mut ctl = DriftController::new(30.0);
        let mut depth = 30.0_f64;
        ctl.set_setpoint(15.0);
        for _ in 0..30_000 {
            let c = ctl.update(depth as f32, 0.02);
            depth += 1000.0 * c * 0.02;
            depth = depth.max(0.0);
        }
        assert!((depth - 15.0).abs() < 1.0, "bufor: {depth:.1} ms");
    }
}
