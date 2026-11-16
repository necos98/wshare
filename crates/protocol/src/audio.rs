//! Audio datagrams: the host's sound, as raw PCM.
//!
//! The format is fixed: [`SAMPLE_RATE`] Hz, [`CHANNELS`] channels,
//! signed 16-bit little-endian samples, interleaved. Uncompressed PCM costs
//! about 1.5 Mbit/s, little on a LAN next to the video, and adds no encoder
//! delay and no codec dependency.
//!
//! The host cuts the sound in chunks of [`CHUNK_FRAMES`] (10 ms), numbered
//! by the position of their first frame: chunk starts are multiples of
//! [`CHUNK_FRAMES`]. A chunk does not fit in one datagram (1920 bytes), so
//! it travels in a few, each carrying whole frames and the position of its
//! first one: every datagram stands on its own, and a lost one silences
//! only its own few milliseconds.
//!
//! Header (little-endian, [`AUDIO_HEADER_LEN`] bytes), then the samples:
//!
//! | off | size | field                                          |
//! |-----|------|------------------------------------------------|
//! | 0   | 1    | marker (= [`AUDIO_MARKER`])                    |
//! | 1   | 1    | flags (none defined yet, ignored)              |
//! | 2   | 2    | stream: changes whenever the capture restarts  |
//! | 4   | 8    | position of the first frame, in frames         |
//!
//! The marker tells audio from video on the shared datagram channel: video
//! datagrams start with [`crate::video::VIDEO_VERSION`], never this value.

use bytes::{BufMut, Bytes, BytesMut};

use crate::ProtocolError;
use crate::video::VIDEO_VERSION;

/// First byte of every audio datagram.
pub const AUDIO_MARKER: u8 = 0xA1;
pub const AUDIO_HEADER_LEN: usize = 12;

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// Bytes of one frame: one 16-bit sample per channel.
pub const FRAME_BYTES: usize = CHANNELS * 2;
/// Frames of one chunk: 10 ms.
pub const CHUNK_FRAMES: usize = (SAMPLE_RATE / 100) as usize;
/// Samples (all channels) of one chunk.
pub const CHUNK_SAMPLES: usize = CHUNK_FRAMES * CHANNELS;

/// What the audio costs on the network, in bit/s: the PCM itself plus,
/// per datagram (two per chunk at the usual MTU), the audio header and
/// about 60 bytes of QUIC, UDP and IP headers.
pub const AUDIO_BANDWIDTH_BPS: u32 = {
    let pcm = SAMPLE_RATE * FRAME_BYTES as u32 * 8;
    let datagrams_per_second = 2 * SAMPLE_RATE / CHUNK_FRAMES as u32;
    let overhead = datagrams_per_second * (AUDIO_HEADER_LEN as u32 + 60) * 8;
    pcm + overhead
};

const _: () = assert!(
    AUDIO_MARKER != VIDEO_VERSION,
    "audio must not look like video"
);

/// Whether `datagram` is an audio datagram (by its first byte only).
pub fn is_audio(datagram: &[u8]) -> bool {
    datagram.first() == Some(&AUDIO_MARKER)
}

/// One audio datagram, parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPacket<'a> {
    pub stream: u16,
    /// Position of the first frame.
    pub position: u64,
    /// Interleaved little-endian 16-bit samples: whole frames, at least one.
    pub pcm: &'a [u8],
}

impl<'a> AudioPacket<'a> {
    /// Parses an audio datagram. Rejects datagrams shorter than the header,
    /// with another marker, or whose samples are not whole frames (or
    /// none).
    pub fn parse(datagram: &'a [u8]) -> Result<Self, ProtocolError> {
        let Some((header, pcm)) = datagram.split_first_chunk::<AUDIO_HEADER_LEN>() else {
            return Err(ProtocolError::Truncated(datagram.len()));
        };
        if header[0] != AUDIO_MARKER {
            return Err(ProtocolError::BadVersion(header[0]));
        }
        if pcm.is_empty() || pcm.len() % FRAME_BYTES != 0 {
            return Err(ProtocolError::Malformed(
                "audio samples are not whole frames",
            ));
        }
        let [_, _, s0, s1, position @ ..] = *header;
        Ok(Self {
            stream: u16::from_le_bytes([s0, s1]),
            position: u64::from_le_bytes(position),
            pcm,
        })
    }

    /// Number of frames carried.
    pub fn frames(&self) -> usize {
        self.pcm.len() / FRAME_BYTES
    }

    /// The samples, decoded, appended to `out`.
    pub fn samples_into(&self, out: &mut Vec<i16>) {
        out.extend(
            self.pcm
                .chunks_exact(2)
                .map(|sample| i16::from_le_bytes([sample[0], sample[1]])),
        );
    }
}

/// Splits a chunk of samples into datagrams of at most `max_datagram_size`
/// bytes, as evenly as whole frames allow. `samples` is interleaved and
/// holds whole frames (a trailing partial frame is not sent). Panics if not
/// even one frame fits after the header.
pub fn packetize(
    stream: u16,
    position: u64,
    samples: &[i16],
    max_datagram_size: usize,
) -> Vec<Bytes> {
    let max_frames = max_datagram_size.saturating_sub(AUDIO_HEADER_LEN) / FRAME_BYTES;
    assert!(
        max_frames > 0,
        "max_datagram_size ({max_datagram_size}) leaves no room for one audio frame"
    );
    let frames = samples.len() / CHANNELS;
    if frames == 0 {
        return Vec::new();
    }
    let count = frames.div_ceil(max_frames);
    let mut buffer = BytesMut::with_capacity(count * AUDIO_HEADER_LEN + frames * FRAME_BYTES);
    let mut datagrams = Vec::with_capacity(count);
    let mut first = 0;
    for index in 0..count {
        // Evenly: the first `frames % count` datagrams carry one frame more.
        let len = frames / count + usize::from(index < frames % count);
        buffer.put_u8(AUDIO_MARKER);
        buffer.put_u8(0);
        buffer.put_u16_le(stream);
        buffer.put_u64_le(position + first as u64);
        for sample in &samples[first * CHANNELS..(first + len) * CHANNELS] {
            buffer.put_i16_le(*sample);
        }
        datagrams.push(buffer.split().freeze());
        first += len;
    }
    datagrams
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoded(datagram: &[u8]) -> (u16, u64, Vec<i16>) {
        let packet = AudioPacket::parse(datagram).unwrap();
        let mut samples = Vec::new();
        packet.samples_into(&mut samples);
        (packet.stream, packet.position, samples)
    }

    #[test]
    fn header_layout_is_stable() {
        let datagrams = packetize(0x0201, 0x0A09_0807_0605_0403, &[-2, 0x0102], 100);
        assert_eq!(datagrams.len(), 1);
        assert_eq!(
            datagrams[0].as_ref(),
            [
                0xA1, 0, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0xFE, 0xFF,
                0x02, 0x01
            ]
        );
    }

    #[test]
    fn a_chunk_is_split_evenly_into_datagrams_that_fit() {
        let samples: Vec<i16> = (0..CHUNK_SAMPLES as i16).collect();
        let datagrams = packetize(3, 960, &samples, 1200);
        assert_eq!(datagrams.len(), 2);
        assert!(datagrams.iter().all(|d| d.len() <= 1200));
        let (stream, first, mut joined) = decoded(&datagrams[0]);
        let (_, second, rest) = decoded(&datagrams[1]);
        assert_eq!((stream, first, second), (3, 960, 960 + 240));
        joined.extend(rest);
        assert_eq!(joined, samples);
    }

    #[test]
    fn uneven_splits_put_the_extra_frames_first() {
        // 5 frames, 2 per datagram at most: 2 + 2 + 1.
        let max = AUDIO_HEADER_LEN + 2 * FRAME_BYTES;
        let datagrams = packetize(0, 0, &[1; 10], max);
        let frames: Vec<_> = datagrams
            .iter()
            .map(|d| AudioPacket::parse(d).unwrap().frames())
            .collect();
        assert_eq!(frames, [2, 2, 1]);
        let positions: Vec<_> = datagrams
            .iter()
            .map(|d| AudioPacket::parse(d).unwrap().position)
            .collect();
        assert_eq!(positions, [0, 2, 4]);
    }

    #[test]
    fn nothing_to_send_gives_no_datagram_and_a_partial_frame_is_dropped() {
        assert!(packetize(0, 0, &[], 1200).is_empty());
        assert!(packetize(0, 0, &[7], 1200).is_empty());
        let datagrams = packetize(0, 0, &[1, 2, 3], 1200);
        assert_eq!(decoded(&datagrams[0]).2, [1, 2]);
    }

    #[test]
    #[should_panic]
    fn a_limit_without_room_for_a_frame_panics() {
        let _ = packetize(0, 0, &[1, 2], AUDIO_HEADER_LEN + FRAME_BYTES - 1);
    }

    #[test]
    fn audio_and_video_datagrams_are_told_apart() {
        let audio = packetize(0, 0, &[1, 2], 100);
        assert!(is_audio(&audio[0]));
        let mut video = Vec::new();
        crate::video::PacketHeader {
            keyframe: true,
            frame_id: 0,
            packet_index: 0,
            packet_count: 1,
            timestamp: 0,
        }
        .write(&mut video);
        assert!(!is_audio(&video));
        assert!(!is_audio(&[]));
        assert!(crate::video::PacketHeader::parse(&audio[0]).is_err());
    }

    #[test]
    fn malformed_datagrams_are_rejected() {
        let good = packetize(0, 0, &[1, 2], 100).remove(0);
        assert_eq!(
            AudioPacket::parse(&good[..AUDIO_HEADER_LEN - 1]),
            Err(ProtocolError::Truncated(AUDIO_HEADER_LEN - 1))
        );
        assert!(matches!(
            AudioPacket::parse(&good[..AUDIO_HEADER_LEN]),
            Err(ProtocolError::Malformed(_))
        ));
        assert!(matches!(
            AudioPacket::parse(&good[..good.len() - 1]),
            Err(ProtocolError::Malformed(_))
        ));
        let mut other = good.to_vec();
        other[0] = VIDEO_VERSION;
        assert_eq!(
            AudioPacket::parse(&other),
            Err(ProtocolError::BadVersion(VIDEO_VERSION))
        );
    }

    #[test]
    fn unknown_flags_are_ignored() {
        let mut datagram = packetize(9, 4, &[5, 6], 100).remove(0).to_vec();
        datagram[1] = 0xFF;
        assert_eq!(decoded(&datagram), (9, 4, vec![5, 6]));
    }

    #[test]
    fn the_format_constants_agree() {
        assert_eq!(CHUNK_FRAMES, 480);
        assert_eq!(
            CHUNK_SAMPLES * 2,
            1920,
            "a chunk needs more than one datagram"
        );
        const { assert!(AUDIO_BANDWIDTH_BPS > 1_536_000 && AUDIO_BANDWIDTH_BPS < 1_800_000) };
    }
}
