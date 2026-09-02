//! Control channel: length-prefixed CBOR over TCP.
//!
//! The sender dials, the receiver listens. Losing this connection invalidates
//! the session and stops the media stream — there is no "keep playing blind".

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

use crate::{PayloadKind, ProtoError, Result};

/// Generous for CBOR control frames, small enough to reject garbage early.
const MAX_FRAME: u32 = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub version: u32,
    pub payload: PayloadKind,
    pub sample_rate: u32,
    pub channels: u16,
    pub frame_ms: u32,
    /// Human-readable source device, shown by the receiver.
    pub device: String,
    /// Human-readable machine name of the sender.
    pub host: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Accept {
    pub version: u32,
    /// Stream identity, carried in every RTP packet.
    pub ssrc: u32,
    /// UDP port the receiver is listening on for media.
    pub media_port: u16,
    /// Human-readable sink device, so the sender can report where audio lands.
    pub sink: String,
    pub host: String,
}

/// Sent by the receiver once a second; the sender uses it to tune bitrate and FEC.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Stats {
    pub lost_pct: f32,
    pub jitter_ms: f32,
    pub buffer_ms: f32,
    pub late_pct: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlMsg {
    Hello(Hello),
    Accept(Accept),
    Reject { reason: String },
    Stats(Stats),
    Mute { on: bool },
    Bye { reason: String },
}

impl ControlMsg {
    pub fn write_to<W: Write>(&self, w: &mut W) -> Result<()> {
        let mut body = Vec::new();
        ciborium::into_writer(self, &mut body).map_err(|e| ProtoError::Encode(e.to_string()))?;
        let len =
            u32::try_from(body.len()).map_err(|_| ProtoError::ControlFrameTooLarge(u32::MAX))?;
        if len > MAX_FRAME {
            return Err(ProtoError::ControlFrameTooLarge(len));
        }
        w.write_all(&len.to_be_bytes())?;
        w.write_all(&body)?;
        w.flush()?;
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> Result<Self> {
        let mut len_buf = [0u8; 4];
        r.read_exact(&mut len_buf)?;
        let len = u32::from_be_bytes(len_buf);
        if len > MAX_FRAME {
            return Err(ProtoError::ControlFrameTooLarge(len));
        }
        let mut body = vec![0u8; len as usize];
        r.read_exact(&mut body)?;
        ciborium::from_reader(&body[..]).map_err(|e| ProtoError::Decode(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_roundtrip() {
        let msgs = vec![
            ControlMsg::Hello(Hello {
                version: 1,
                payload: PayloadKind::PcmS16,
                sample_rate: 48_000,
                channels: 1,
                frame_ms: 10,
                device: "Yeti Nano".into(),
                host: "laptop".into(),
            }),
            ControlMsg::Stats(Stats {
                lost_pct: 1.5,
                jitter_ms: 4.0,
                buffer_ms: 30.0,
                late_pct: 0.1,
            }),
            ControlMsg::Mute { on: true },
        ];

        let mut buf = Vec::new();
        for m in &msgs {
            m.write_to(&mut buf).unwrap();
        }

        let mut cursor = &buf[..];
        for expected in &msgs {
            let got = ControlMsg::read_from(&mut cursor).unwrap();
            assert_eq!(format!("{got:?}"), format!("{expected:?}"));
        }
    }

    #[test]
    fn rejects_absurd_length() {
        let mut framed = Vec::new();
        framed.extend_from_slice(&(MAX_FRAME + 1).to_be_bytes());
        let err = ControlMsg::read_from(&mut &framed[..]).unwrap_err();
        assert!(matches!(err, ProtoError::ControlFrameTooLarge(_)));
    }
}
