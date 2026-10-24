//! Jitter from a seed (xorshift64*): repeatable in tests, enough to spread
//! clients apart.

pub(crate) struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        // splitmix64: nearby seeds give unrelated streams.
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        Rng((z ^ (z >> 31)) | 1)
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `0..=max`.
    pub fn upto(&mut self, max: i64) -> i64 {
        (self.next() % (max.max(0) as u64 + 1)) as i64
    }

    /// An id for local things (operations, client_msg_id): the time
    /// first, so two processes started with the same seed still differ.
    pub fn id(&mut self, now: i64) -> String {
        format!(
            "{:012x}{:016x}{:04x}",
            now,
            self.next(),
            self.next() & 0xffff
        )
    }
}
