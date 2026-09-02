//! Minimal RTP header: version 2, no padding, no extension, no CSRCs.
//!
//! Layout (RFC 3550 §5.1), 12 bytes:
//! ```text
//!  0                   1                   2                   3
//!  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |V=2|P|X|  CC   |M|     PT      |       sequence number         |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                           timestamp                           |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                             SSRC                              |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! ```

use crate::{ProtoError, Result};

pub const RTP_HEADER_LEN: usize = 12;

/// Payload types. 111 is the de-facto dynamic type for Opus/48000 used by
/// WebRTC, which makes third-party tools decode our stream without a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum PayloadKind {
    /// 16-bit signed PCM, network (big-endian) byte order, mono. Milestone 1 only.
    PcmS16 = 96,
    /// Opus, 48 kHz.
    Opus = 111,
}

impl PayloadKind {
    pub fn from_u8(v: u8) -> Result<Self> {
        match v {
            96 => Ok(PayloadKind::PcmS16),
            111 => Ok(PayloadKind::Opus),
            other => Err(ProtoError::UnknownPayloadType(other)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtpHeader {
    /// Set on the first packet of a talkspurt after silence (DTX).
    pub marker: bool,
    pub payload: PayloadKind,
    pub seq: u16,
    /// In 1/48000 s units; advances by FRAME_SAMPLES per frame.
    pub timestamp: u32,
    pub ssrc: u32,
}

impl RtpHeader {
    pub fn encode_into(&self, out: &mut [u8]) -> Result<()> {
        if out.len() < RTP_HEADER_LEN {
            return Err(ProtoError::TooShort {
                got: out.len(),
                want: RTP_HEADER_LEN,
            });
        }
        out[0] = 0b1000_0000; // V=2, P=0, X=0, CC=0
        out[1] = (self.payload as u8) | if self.marker { 0x80 } else { 0 };
        out[2..4].copy_from_slice(&self.seq.to_be_bytes());
        out[4..8].copy_from_slice(&self.timestamp.to_be_bytes());
        out[8..12].copy_from_slice(&self.ssrc.to_be_bytes());
        Ok(())
    }

    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < RTP_HEADER_LEN {
            return Err(ProtoError::TooShort {
                got: buf.len(),
                want: RTP_HEADER_LEN,
            });
        }
        if buf[0] >> 6 != 2 {
            return Err(ProtoError::BadVersion);
        }
        Ok(RtpHeader {
            marker: buf[1] & 0x80 != 0,
            payload: PayloadKind::from_u8(buf[1] & 0x7f)?,
            seq: u16::from_be_bytes([buf[2], buf[3]]),
            timestamp: u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]),
            ssrc: u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]),
        })
    }
}

/// Write mono i16 samples in network byte order, returning the byte count.
pub fn encode_pcm(samples: &[i16], out: &mut [u8]) -> usize {
    for (chunk, s) in out.chunks_exact_mut(2).zip(samples) {
        chunk.copy_from_slice(&s.to_be_bytes());
    }
    samples.len() * 2
}

/// Read mono i16 samples from network byte order. A trailing odd byte is ignored.
pub fn decode_pcm(bytes: &[u8], out: &mut Vec<i16>) {
    out.clear();
    out.reserve(bytes.len() / 2);
    for chunk in bytes.chunks_exact(2) {
        out.push(i16::from_be_bytes([chunk[0], chunk[1]]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let h = RtpHeader {
            marker: true,
            payload: PayloadKind::Opus,
            seq: 0xBEEF,
            timestamp: 0x0102_0304,
            ssrc: 0x7A31_F0C2,
        };
        let mut buf = [0u8; RTP_HEADER_LEN];
        h.encode_into(&mut buf).unwrap();
        assert_eq!(RtpHeader::decode(&buf).unwrap(), h);
    }

    #[test]
    fn header_is_rtp_v2_on_the_wire() {
        let h = RtpHeader {
            marker: false,
            payload: PayloadKind::PcmS16,
            seq: 1,
            timestamp: 480,
            ssrc: 42,
        };
        let mut buf = [0u8; RTP_HEADER_LEN];
        h.encode_into(&mut buf).unwrap();
        assert_eq!(buf[0], 0x80, "version 2, no padding/extension/CSRC");
        assert_eq!(buf[1], 96, "payload type in the low 7 bits, marker clear");
    }

    #[test]
    fn rejects_short_and_wrong_version() {
        assert!(matches!(
            RtpHeader::decode(&[0u8; 4]),
            Err(ProtoError::TooShort { .. })
        ));
        let mut buf = [0u8; RTP_HEADER_LEN];
        buf[0] = 0x40; // version 1
        assert!(matches!(
            RtpHeader::decode(&buf),
            Err(ProtoError::BadVersion)
        ));
    }

    #[test]
    fn pcm_roundtrip() {
        let samples: Vec<i16> = (-4..4).map(|v| v * 1000).collect();
        let mut bytes = vec![0u8; samples.len() * 2];
        let n = encode_pcm(&samples, &mut bytes);
        assert_eq!(n, bytes.len());
        let mut back = Vec::new();
        decode_pcm(&bytes, &mut back);
        assert_eq!(back, samples);
    }
}
