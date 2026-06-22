//! Deterministic integer hashing for the Dots sketch's seeded systems.
//!
//! The spawn-time particle seeding ([`crate::dots::systems::spawn`]) needs
//! *stateless, reproducible* pseudo-randomness: the same input always hashes to
//! the same value, on every platform, with no RNG state. That is what keeps
//! particle lifespans and the fraction-kill survivor set capture-reproducible.
//!
//! Shared candidate with [`crate::line::hash`] — the two modules are currently
//! duplicated so each sketch remains self-contained. A future refactor could
//! promote the shared primitives to `wc-core`.

/// Wang's 32-bit integer mix.
///
/// Deterministic and stateless — see the module docs for why the Dots sketch
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

/// Shortest attract-mode lifespan a Dots particle can be seeded with, in seconds.
///
/// Matching Line's [`crate::line::systems::spawn::ATTRACT_LIFESPAN_MIN_SECS`] so
/// the two sketches have consistent screensaver renewal rates.
pub const DOTS_ATTRACT_LIFESPAN_MIN_SECS: f32 = 10.0;

/// Longest attract-mode lifespan a Dots particle can be seeded with, in seconds.
///
/// Matching Line's [`crate::line::systems::spawn::ATTRACT_LIFESPAN_MAX_SECS`] so
/// the two sketches have consistent screensaver renewal rates.
pub const DOTS_ATTRACT_LIFESPAN_MAX_SECS: f32 = 18.0;

/// Salt XOR-ed into the index before hashing for [`dots_attract_lifespan`], so
/// the lifespan stream is decorrelated from the [`spawn_hash01`] stream
/// (otherwise the fraction kill would preferentially cull one end of the
/// lifespan range). Same value as
/// `crate::line::systems::spawn::LIFESPAN_HASH_SALT`; kept separate so the
/// sketches stay self-contained.
const LIFESPAN_HASH_SALT: u32 = 0x9E37_79B9;

/// Deterministic per-index hash in `0..=1`, seeded into
/// [`crate::particles::particle::Particle::spawn_hash`] at spawn. The
/// attract-mode fraction gate kills particles with `spawn_hash >= attract_fraction`;
/// hashing the index (rather than comparing the index itself) makes the cull
/// spatially uniform across the grid.
#[must_use]
pub fn spawn_hash01(i: u32) -> f32 {
    hash_to_unit(wang_hash(i))
}

/// Deterministic attract-mode lifespan for particle `i`, uniform in
/// [`DOTS_ATTRACT_LIFESPAN_MIN_SECS`]..=[`DOTS_ATTRACT_LIFESPAN_MAX_SECS`].
/// Seeded into [`crate::particles::particle::Particle::lifespan`] at spawn.
/// Per-particle staggering means the attract field self-heals continuously
/// instead of respawning in visible waves.
#[must_use]
pub fn dots_attract_lifespan(i: u32) -> f32 {
    let unit = hash_to_unit(wang_hash(i ^ LIFESPAN_HASH_SALT));
    DOTS_ATTRACT_LIFESPAN_MIN_SECS
        + (DOTS_ATTRACT_LIFESPAN_MAX_SECS - DOTS_ATTRACT_LIFESPAN_MIN_SECS) * unit
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

    #[test]
    fn spawn_hash_is_uniform_enough_for_the_fraction_gate() {
        // The fraction gate keeps particles with spawn_hash < fraction; the
        // hash must be roughly uniform so a 0.6 fraction keeps ~60% of the
        // field, evenly across index (and therefore screen position) order.
        let n = 10_000_u32;
        let fraction = 0.6_f32;
        let mut kept = 0_u32;
        // Also count survivors in the left and right index halves to confirm
        // the hash is spatially unbiased.
        let mut kept_left = 0_u32;
        for i in 0..n {
            let h = spawn_hash01(i);
            assert!((0.0..=1.0).contains(&h), "hash {h} out of unit range");
            if h < fraction {
                kept += 1;
                if i < n / 2 {
                    kept_left += 1;
                }
            }
        }
        let kept_frac = f64::from(kept) / f64::from(n);
        assert!(
            (kept_frac - 0.6).abs() < 0.03,
            "kept fraction {kept_frac} should be ~0.6"
        );
        let left_share = f64::from(kept_left) / f64::from(kept);
        assert!(
            (left_share - 0.5).abs() < 0.03,
            "survivors should be index-uniform, left share = {left_share}"
        );
    }

    #[test]
    fn attract_lifespan_is_deterministic_and_in_range() {
        for i in 0..10_000_u32 {
            let a = dots_attract_lifespan(i);
            let b = dots_attract_lifespan(i);
            assert!(a.to_bits() == b.to_bits(), "lifespan must be deterministic");
            assert!(
                (DOTS_ATTRACT_LIFESPAN_MIN_SECS..=DOTS_ATTRACT_LIFESPAN_MAX_SECS).contains(&a),
                "lifespan {a} out of range at index {i}"
            );
        }
    }

    #[test]
    fn attract_lifespans_are_staggered() {
        // Over a typical buffer the seeded values must spread across (not cluster
        // within) the range. Check the mean sits near the midpoint and both
        // tails are reached.
        let n = 10_000_u32;
        let mut min = f32::MAX;
        let mut max = f32::MIN;
        let mut sum = 0.0_f64;
        for i in 0..n {
            let l = dots_attract_lifespan(i);
            min = min.min(l);
            max = max.max(l);
            sum += f64::from(l);
        }
        let mean = sum / f64::from(n);
        let mid =
            f64::from(DOTS_ATTRACT_LIFESPAN_MIN_SECS + DOTS_ATTRACT_LIFESPAN_MAX_SECS) / 2.0;
        assert!(
            (mean - mid).abs() < 1.0,
            "lifespan mean {mean} far from {mid}"
        );
        assert!(
            min < DOTS_ATTRACT_LIFESPAN_MIN_SECS + 2.0,
            "low tail unreached: {min}"
        );
        assert!(
            max > DOTS_ATTRACT_LIFESPAN_MAX_SECS - 2.0,
            "high tail unreached: {max}"
        );
    }
}
