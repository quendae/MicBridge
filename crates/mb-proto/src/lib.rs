//! Wire format for MicBridge: RTP-compatible media framing plus a small
//! length-prefixed CBOR control protocol.
//!
//! The media header is deliberately a real RTP header so a live stream can be
//! decoded by Wireshark and standard RTP tooling while debugging.

pub mod control;
pub mod rtp;
pub mod seq;

pub use control::{read_frame, write_frame, Accept, ControlMsg, Hello, Init, Stats};
pub use rtp::{PayloadKind, RtpHeader, RTP_HEADER_LEN};
pub use seq::SeqExtender;

/// TCP port for the control channel (handshake, stats, mute, teardown).
pub const CONTROL_PORT: u16 = 47100;
/// UDP port carrying RTP media.
pub const MEDIA_PORT: u16 = 47101;

/// Bumped on any incompatible change to the control protocol.
///
/// 2: sesja zaczyna się od rozpoznania i parowania, a media są szyfrowane.
/// Starszy nadajnik nie ma jak się dogadać z nowszym odbiornikiem, więc
/// rozbieżność wyłapujemy już na liście w `discover`.
pub const PROTOCOL_VERSION: u32 = 2;

/// Everything in the engine runs at this rate; devices that disagree are resampled.
pub const SAMPLE_RATE: u32 = 48_000;
/// Frame duration on the wire. 10 ms is the sweet spot between overhead and latency.
pub const FRAME_MS: u32 = 10;
/// Samples per mono frame: 480 at 48 kHz / 10 ms.
pub const FRAME_SAMPLES: usize = (SAMPLE_RATE * FRAME_MS / 1000) as usize;

#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("packet too short: {got} B, expected at least {want} B")]
    TooShort { got: usize, want: usize },
    #[error("not an RTP version 2 packet")]
    BadVersion,
    #[error("unknown payload type {0}")]
    UnknownPayloadType(u8),
    #[error("control frame of {0} B exceeds the limit")]
    ControlFrameTooLarge(u32),
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("CBOR decode: {0}")]
    Decode(String),
    #[error("CBOR encode: {0}")]
    Encode(String),
}

pub type Result<T> = std::result::Result<T, ProtoError>;
