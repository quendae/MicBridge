//! Operating-system-independent core: jitter buffer, statistics, and (from
//! milestone 2) the codec and clock-drift controller.
//!
//! Nothing here touches a sound card or a socket, so the whole module can be
//! driven from tests with a synthetic packet stream.

pub mod jitter;
pub mod stats;

pub use jitter::{JitterBuffer, Pop};
pub use stats::StreamStats;
