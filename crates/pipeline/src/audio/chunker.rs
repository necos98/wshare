//! Cuts the captured sound, which arrives in whatever amounts the device
//! delivers, into chunks of exactly [`CHUNK_FRAMES`], numbered by the
//! position of their first frame (see [`protocol::audio`]).

use protocol::audio::{CHUNK_FRAMES, CHUNK_SAMPLES};

/// Collects samples until a chunk is full.
#[derive(Debug, Default)]
pub struct Chunker {
    pending: Vec<i16>,
    /// Position of the first frame of the chunk being collected.
    position: u64,
}

impl Chunker {
    pub fn new() -> Self {
        Self {
            pending: Vec::with_capacity(CHUNK_SAMPLES),
            position: 0,
        }
    }

    /// Adds interleaved `samples` and hands every chunk they complete to
    /// `emit`, with its position. Chunk starts are multiples of
    /// [`CHUNK_FRAMES`]; positions go on counting across calls.
    pub fn push(&mut self, mut samples: &[i16], mut emit: impl FnMut(u64, &[i16])) {
        while !samples.is_empty() {
            let take = (CHUNK_SAMPLES - self.pending.len()).min(samples.len());
            self.pending.extend_from_slice(&samples[..take]);
            samples = &samples[take..];
            if self.pending.len() == CHUNK_SAMPLES {
                emit(self.position, &self.pending);
                self.pending.clear();
                self.position += CHUNK_FRAMES as u64;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(chunker: &mut Chunker, samples: &[i16]) -> Vec<(u64, Vec<i16>)> {
        let mut chunks = Vec::new();
        chunker.push(samples, |position, chunk| {
            chunks.push((position, chunk.to_vec()));
        });
        chunks
    }

    #[test]
    fn small_deliveries_add_up_to_whole_chunks() {
        let mut chunker = Chunker::new();
        let third = CHUNK_SAMPLES / 3;
        assert!(collect(&mut chunker, &vec![1; third]).is_empty());
        assert!(collect(&mut chunker, &vec![2; third]).is_empty());
        let chunks = collect(&mut chunker, &vec![3; CHUNK_SAMPLES - 2 * third + 4]);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].0, 0);
        assert_eq!(chunks[0].1.len(), CHUNK_SAMPLES);
        assert_eq!(chunks[0].1[0], 1);
        assert_eq!(*chunks[0].1.last().unwrap(), 3);
        // The 4 extra samples start the next chunk.
        let next = collect(&mut chunker, &vec![4; CHUNK_SAMPLES - 4]);
        assert_eq!(next[0].0, CHUNK_FRAMES as u64);
        assert_eq!(&next[0].1[..4], [3, 3, 3, 3]);
    }

    #[test]
    fn a_large_delivery_gives_several_chunks_in_order() {
        let mut chunker = Chunker::new();
        let chunks = collect(&mut chunker, &vec![0; CHUNK_SAMPLES * 3 + 2]);
        let positions: Vec<_> = chunks.iter().map(|(position, _)| *position).collect();
        let step = CHUNK_FRAMES as u64;
        assert_eq!(positions, [0, step, 2 * step]);
    }
}
