//! Deterministic integer hashing shared by the Line sketch's seeded systems.
//!
//! The spawn-time particle seeding ([`crate::line::systems::spawn`]) needs
//! *stateless, reproducible* pseudo-randomness: the same input always hashes to
//! the same value, on every platform, with no RNG state. That is what keeps
//! particle lifespans and the fraction-kill survivor set capture-reproducible.

/// Wang's 32-bit integer mix.
///
/// Deterministic and stateless — see the module docs for why the Line sketch
/// hashes instead of sampling an RNG.
#[must_use]
pub fn wang_hash(mut x: u32) -> u32 {
    x = (x ^ 0x3D) ^ (x >> 16); // 0x3D = Wang's published constant 61
    x = x.wrapping_mul(9);
    x ^= x >> 4;
    x = x.wrapping_mul(0x27d4_eb2d);
    x ^ (x >> 15)
}

/// Map a hashed `u32` onto `0..=1`.
#[must_use]
#[allow(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "u32 -> f32 unit mapping; sub-ulp precision loss is irrelevant for seeding"
)]
pub fn hash_to_unit(h: u32) -> f32 {
    h as f32 / u32::MAX as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wang_hash_is_deterministic_and_mixing() {
        assert_eq!(wang_hash(42), wang_hash(42));
        // Neighboring inputs decorrelate (the property the seeding relies on).
        assert_ne!(wang_hash(0), wang_hash(1));
        assert_ne!(wang_hash(1), wang_hash(2));
    }

    #[test]
    fn hash_to_unit_stays_in_range() {
        for i in 0..1_000_u32 {
            let u = hash_to_unit(wang_hash(i));
            assert!((0.0..=1.0).contains(&u), "got {u}");
        }
    }
}
