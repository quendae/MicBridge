//! Operating-system-independent core: jitter buffer, statistics, and (from
//! milestone 2) the codec and clock-drift controller.
//!
//! Nothing here touches a sound card or a socket, so the whole module can be
//! driven from tests with a synthetic packet stream.

pub mod codec;
pub mod drift;
pub mod jitter;
pub mod netsim;
pub mod resample;
pub mod stats;

pub use codec::{OpusDecoder, OpusEncoder};
pub use drift::DriftController;
pub use jitter::{AdaptiveTarget, JitterBuffer, Pop};
pub use resample::VariableResampler;
pub use stats::StreamStats;
