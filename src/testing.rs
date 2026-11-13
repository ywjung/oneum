//! Test support: a deterministic source of damaged input, for the tests that feed the
//! parsers what a broken or hostile file would.

/// xorshift64*: the same seed gives the same input, so a failure can be repeated.
pub(crate) struct Noise(u64);

impl Noise {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub(crate) fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A number below `n` (0 when `n` is 0).
    pub(crate) fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }

    pub(crate) fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| (self.next() >> 24) as u8).collect()
    }

    /// A copy of `good` damaged the way files are: bytes changed, a stretch cut out,
    /// repeated or overwritten, the end missing, or a length field set to an extreme.
    pub(crate) fn damaged(&mut self, good: &[u8]) -> Vec<u8> {
        let mut bytes = good.to_vec();
        for _ in 0..1 + self.below(4) {
            if bytes.is_empty() {
                break;
            }
            let at = self.below(bytes.len());
            let span = 1 + self.below(64).min(bytes.len() - at - 1);
            match self.below(7) {
                0 => bytes[at] = (self.next() >> 16) as u8,
                1 => bytes[at] ^= 1 << self.below(8),
                2 => {
                    bytes.drain(at..at + span);
                }
                3 => {
                    let copy = bytes[at..at + span].to_vec();
                    bytes.splice(at..at, copy);
                }
                4 => {
                    let noise = self.bytes(span);
                    bytes[at..at + span].copy_from_slice(&noise);
                }
                5 => bytes.truncate(at),
                _ => {
                    // The values length fields overflow on.
                    let extreme: [u8; 4] = match self.below(4) {
                        0 => [0xFF; 4],
                        1 => [0; 4],
                        2 => [0x7F, 0xFF, 0xFF, 0xFF],
                        _ => [0x80, 0, 0, 0],
                    };
                    let end = (at + 4).min(bytes.len());
                    bytes[at..end].copy_from_slice(&extreme[..end - at]);
                }
            }
        }
        bytes
    }

    /// `count` of the `pieces` in a random order, joined without separators: text that
    /// has the vocabulary of a format and none of its order.
    pub(crate) fn text(&mut self, pieces: &[&str], count: usize) -> String {
        (0..count)
            .map(|_| pieces[self.below(pieces.len())])
            .collect()
    }
}
