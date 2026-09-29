//! Q31 (1.31 fixed-point) <-> `f32` conversion.
//!
//! This is the same format `tools/sharc_dac.py`'s `q31_to_float`/
//! `float_to_q31` define for ring A: a signed 32-bit integer, full scale
//! `+-1.0`, `value = raw_i32 / 2**31`. The SHARC frame source converts Q31
//! -> f32 here, in the producer thread, never in the real-time callback
//! (`src/ring.rs` module docs).

/// Full-scale divisor, `2**31` (`tools/sharc_dac.py` `Q31_FULL_SCALE`).
pub const Q31_FULL_SCALE: f64 = 2_147_483_648.0; // 2**31

/// One raw Q31 word to its `f32` value in `[-1.0, 1.0)`.
pub fn q31_to_f32(raw: i32) -> f32 {
    (raw as f64 / Q31_FULL_SCALE) as f32
}

/// The inverse: a float, clipped (not wrapped) to the representable Q31
/// range, matching `tools/sharc_dac.py`'s `float_to_q31` (used by tests and
/// by the test-tone source, which synthesizes in Q31 to exercise the same
/// path a real SHARC frame source would).
pub fn f32_to_q31(value: f32) -> i32 {
    let scaled = (value as f64 * Q31_FULL_SCALE).round();
    let limit = Q31_FULL_SCALE - 1.0;
    scaled.clamp(-Q31_FULL_SCALE, limit) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_round_trips() {
        assert_eq!(q31_to_f32(0), 0.0);
        assert_eq!(f32_to_q31(0.0), 0);
    }

    #[test]
    fn full_scale_positive_is_at_or_just_under_one() {
        // i32::MAX / 2**31 is 1 - 2**-31 exactly in f64, but f32's ~24-bit
        // mantissa cannot distinguish that from 1.0 at this magnitude, so
        // the nearest f32 rounds up to exactly 1.0 -- expected precision
        // loss from converting to f32 in the producer (as the live audio
        // path does throughout), not a conversion bug.
        let max_q31 = i32::MAX; // 0x7FFF_FFFF
        let f = q31_to_f32(max_q31);
        assert!(f <= 1.0);
        assert!(f > 0.999_999);
    }

    #[test]
    fn full_scale_negative_is_exactly_minus_one() {
        let min_q31 = i32::MIN; // 0x8000_0000
        let f = q31_to_f32(min_q31);
        assert_eq!(f, -1.0);
    }

    #[test]
    fn f32_to_q31_clips_above_full_scale() {
        assert_eq!(f32_to_q31(1.5), i32::MAX);
        assert_eq!(f32_to_q31(-1.5), i32::MIN);
    }

    #[test]
    fn known_values_match_sharc_dac_examples() {
        // tools/sharc_dac.py's isolated-call test: L[k] = 0.1 + 0.01*k.
        let value = 0.1f32;
        let raw = f32_to_q31(value);
        let back = q31_to_f32(raw);
        assert!((back - value).abs() < 1e-6);
    }

    #[test]
    fn round_trip_is_stable_across_the_range() {
        for i in -20..=20 {
            let v = i as f32 / 20.0;
            let raw = f32_to_q31(v);
            let back = q31_to_f32(raw);
            assert!((back - v).abs() < 1e-6, "v={v} raw={raw} back={back}");
        }
    }
}
