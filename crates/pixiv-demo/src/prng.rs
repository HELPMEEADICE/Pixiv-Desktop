//! Deterministic pseudo-random numbers for the demo catalogue.
//!
//! SplitMix64 is used rather than a `rand` dependency: three finalising
//! `wrapping_mul`/xor steps turn a single `u64` seed into a full-period stream,
//! so every value the catalogue contains is a pure function of [`crate::catalog::SEED`].
//! A dependency upgrade can therefore never shift the demo data, and there is no
//! entropy source in the build at all.

/// SplitMix64 generator state.
#[derive(Debug, Clone)]
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) const fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        mix_advance(&mut self.0)
    }

    /// Value in `lo..hi`; a degenerate or reversed range yields `lo`.
    pub(crate) fn range(&mut self, lo: u32, hi: u32) -> u32 {
        if hi <= lo {
            return lo;
        }
        let span = u64::from(hi - lo);
        lo + u32::try_from(self.next_u64() % span).unwrap_or(0)
    }

    /// Value in `0..n`; `n == 0` yields `0`.
    pub(crate) fn below(&mut self, n: u32) -> u32 {
        self.range(0, n)
    }
}

/// The SplitMix64 finaliser, exposed so callers can map an arbitrary key (an
/// illustration id, say) to a well-spread `u64` without carrying state.
pub(crate) fn mix(mut value: u64) -> u64 {
    mix_advance(&mut value)
}

fn mix_advance(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::Rng;

    #[test]
    fn same_seed_yields_the_same_stream() {
        let mut a = Rng::new(12345);
        let mut b = Rng::new(12345);
        for _ in 0..64 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn range_stays_inside_bounds() {
        let mut rng = Rng::new(1);
        for _ in 0..512 {
            let v = rng.range(3, 9);
            assert!((3..9).contains(&v), "value {v} escaped 3..9");
        }
        assert_eq!(rng.range(5, 5), 5);
        assert_eq!(rng.range(9, 2), 9);
        assert_eq!(rng.below(0), 0);
    }
}
