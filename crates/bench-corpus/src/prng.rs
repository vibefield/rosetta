//! Deterministic xorshift64\* PRNG.
//!
//! No `rand`, no OS entropy, no clock: the corpus must be byte-identical on
//! every machine from `--seed` alone (CLAUDE.md iron rule 3). Statistical
//! quality only has to be good enough to shape a fixture — modulo bias in
//! [`Rng::below`] is deliberate and harmless here.

/// Fractional part of the golden ratio in 64 bits — the standard mixing
/// constant for deriving independent streams from one seed.
pub const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

const XORSHIFT_MULTIPLIER: u64 = 0x2545_F491_4F6C_DD1D;

pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        let state = seed ^ GOLDEN;
        Self {
            // xorshift is a fixed point at zero; any other start is fine.
            state: if state == 0 {
                XORSHIFT_MULTIPLIER
            } else {
                state
            },
        }
    }

    /// An independent stream derived from this seed and a salt. Used to give
    /// every session its own stream, so a session's bytes depend on its index
    /// and the seed alone — never on what was generated before it.
    pub fn stream(seed: u64, salt: u64) -> Self {
        Self::new(seed ^ salt.wrapping_mul(GOLDEN))
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(XORSHIFT_MULTIPLIER)
    }

    /// Uniform-ish in `[0, n)`; `0` when `n == 0`.
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }

    /// Uniform-ish in `[lo, hi)`; `lo` when the range is empty.
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi.saturating_sub(lo))
    }

    /// Uniform in `[0, 1)` with 53 bits of resolution.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn chance(&mut self, probability: f64) -> bool {
        self.unit() < probability
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }

    pub fn hex(&mut self, digits: usize) -> String {
        let mut out = String::with_capacity(digits);
        while out.len() < digits {
            out.push_str(&format!("{:016x}", self.next_u64()));
        }
        out.truncate(digits);
        out
    }

    /// A UUID-shaped string with the given version nibble and the RFC 4122
    /// variant bits. claude-code sessions look like v4, codex threads like v7 —
    /// the bytes are synthetic either way.
    pub fn uuid(&mut self, version: u8) -> String {
        let mut bytes = [0u8; 16];
        bytes[..8].copy_from_slice(&self.next_u64().to_be_bytes());
        bytes[8..].copy_from_slice(&self.next_u64().to_be_bytes());
        bytes[6] = (bytes[6] & 0x0f) | (version << 4);
        bytes[8] = (bytes[8] & 0x3f) | 0x80;

        let mut out = String::with_capacity(36);
        for (index, byte) in bytes.iter().enumerate() {
            if matches!(index, 4 | 6 | 8 | 10) {
                out.push('-');
            }
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::Rng;

    #[test]
    fn same_seed_same_stream() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        let mut c = Rng::new(43);
        let left: Vec<u64> = (0..8).map(|_| a.next_u64()).collect();
        let right: Vec<u64> = (0..8).map(|_| b.next_u64()).collect();
        let other: Vec<u64> = (0..8).map(|_| c.next_u64()).collect();
        assert_eq!(left, right);
        assert_ne!(left, other);
    }

    #[test]
    fn the_zero_state_is_escaped() {
        // Seeding with GOLDEN cancels the mixing constant to a zero state,
        // which is xorshift's fixed point — the constructor must divert it.
        let mut rng = Rng::new(super::GOLDEN);
        assert_ne!(rng.next_u64(), 0);
        assert_ne!(rng.next_u64(), 0);
    }

    #[test]
    fn uuid_is_shaped_correctly() {
        let mut rng = Rng::new(7);
        let id = rng.uuid(4);
        assert_eq!(id.len(), 36);
        let groups: Vec<&str> = id.split('-').collect();
        assert_eq!(
            groups.iter().map(|g| g.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12]
        );
        assert!(id.chars().all(|c| c == '-' || c.is_ascii_hexdigit()));
        assert!(groups[2].starts_with('4'));
        assert!(matches!(
            groups[3].chars().next(),
            Some('8' | '9' | 'a' | 'b')
        ));
    }

    #[test]
    fn ranges_stay_in_bounds() {
        let mut rng = Rng::new(1);
        for _ in 0..1000 {
            let value = rng.range(10, 20);
            assert!((10..20).contains(&value));
            let unit = rng.unit();
            assert!((0.0..1.0).contains(&unit));
        }
        assert_eq!(rng.range(5, 5), 5);
        assert_eq!(rng.below(0), 0);
    }
}
