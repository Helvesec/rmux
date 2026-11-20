//! Hexadecimal operands that carry a fraction or a binary exponent.
//!
//! `strtod` accepts `0x1.8p+1`, `0X1.CP-1` or `0x.8` and refuses anything it
//! cannot consume entirely (format.c:4973-4983 hands it each operand and rejects
//! a leftover suffix). Rust's own `f64` parser does not read those forms and the
//! prefixed-integer path only reads whole hexadecimal digits, so this module
//! covers that one domain and nothing else: it answers `None` for every shape
//! without a point and without a `p`, which leaves the decimal and integer paths
//! exactly where they were.
//!
//! The conversion is done in integers and rounded once. The digits are gathered
//! into a 128-bit significand with a sticky bit for anything dropped, the
//! binary exponent is accumulated as a count rather than by scaling, and the
//! result is rounded to the precision its own exponent allows - 53 bits when it
//! is normal, fewer when it lands in the subnormals - so nothing is rounded
//! twice. No FFI, no locale, no new dependency, and the work is bounded by the
//! length of the text rather than by the value of the exponent.

/// The significand keeps at most this many bits; anything beyond only sets the
/// sticky bit, which is all the rounding needs.
const SIGNIFICAND_BITS: u32 = 120;
const MAX_EXPONENT: i64 = 1023;
const MIN_EXPONENT: i64 = -1022;
const MANTISSA_BITS: i64 = 53;
/// Past this the exponent can only saturate, so the text stops being read as a
/// number long before any arithmetic could overflow.
const EXPONENT_LIMIT: i64 = 1 << 40;

pub(in crate::formats) fn parse_hexadecimal_float(value: &str) -> Option<f64> {
    let (negative, rest) = match value.as_bytes().first().copied() {
        Some(b'-') => (true, &value[1..]),
        Some(b'+') => (false, &value[1..]),
        _ => (false, value),
    };
    let rest = rest
        .strip_prefix("0x")
        .or_else(|| rest.strip_prefix("0X"))?;

    let bytes = rest.as_bytes();
    let mut index = 0;
    let mut significand: u128 = 0;
    let mut bits: u32 = 0;
    let mut sticky = false;
    let mut exponent: i64 = 0;
    let mut digits = 0_usize;
    let mut fraction_digits = 0_i64;
    let mut seen_point = false;

    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'.' {
            if seen_point {
                return None;
            }
            seen_point = true;
            index += 1;
            continue;
        }
        let Some(digit) = (byte as char).to_digit(16) else {
            break;
        };
        digits += 1;
        if seen_point {
            fraction_digits += 1;
        }
        if significand == 0 && digit == 0 {
            // Leading zeros carry no bits; only their place value matters, and
            // that is already counted by `fraction_digits`.
            index += 1;
            continue;
        }
        if bits + 4 <= SIGNIFICAND_BITS {
            significand = (significand << 4) | u128::from(digit);
            bits += 4;
        } else {
            sticky |= digit != 0;
            exponent += 4;
        }
        index += 1;
    }

    if digits == 0 {
        // `0x.p1` and `0x` alone name no significand at all.
        return None;
    }
    // This module answers only for the new domain: a plain hexadecimal integer
    // stays on the path it already had.
    let has_exponent_marker = matches!(bytes.get(index), Some(b'p' | b'P'));
    if !seen_point && !has_exponent_marker {
        return None;
    }
    exponent -= fraction_digits * 4;

    if has_exponent_marker {
        index += 1;
        let (exponent_negative, exponent_digits) = match bytes.get(index) {
            Some(b'-') => {
                index += 1;
                (true, &bytes[index..])
            }
            Some(b'+') => {
                index += 1;
                (false, &bytes[index..])
            }
            _ => (false, &bytes[index..]),
        };
        if exponent_digits.is_empty() || !exponent_digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        index += exponent_digits.len();
        let mut written: i64 = 0;
        for byte in exponent_digits {
            written = written
                .saturating_mul(10)
                .saturating_add(i64::from(byte - b'0'));
            if written > EXPONENT_LIMIT {
                written = EXPONENT_LIMIT;
                break;
            }
        }
        exponent = exponent.saturating_add(if exponent_negative { -written } else { written });
    }

    if index != bytes.len() {
        // strtod refuses an operand it cannot consume entirely.
        return None;
    }

    Some(assemble(negative, significand, sticky, exponent))
}

/// Rounds the significand once, at the precision its own exponent allows, and
/// puts the result together.
fn assemble(negative: bool, significand: u128, sticky: bool, exponent: i64) -> f64 {
    let sign = if negative { -1.0_f64 } else { 1.0_f64 };
    if significand == 0 {
        // A significand of zero is a zero whatever the exponent says, however
        // large or small it is, and it keeps its sign.
        return sign * 0.0;
    }

    let significant_bits = i64::from(128 - significand.leading_zeros());
    // The exponent the result would have once normalised.
    let normalised = exponent + significant_bits - 1;
    if normalised > MAX_EXPONENT {
        return sign * f64::INFINITY;
    }
    if normalised < MIN_EXPONENT - MANTISSA_BITS - 1 {
        return sign * 0.0;
    }

    let precision = if normalised < MIN_EXPONENT {
        // A subnormal result keeps only the bits its exponent leaves room for.
        MANTISSA_BITS - (MIN_EXPONENT - normalised)
    } else {
        MANTISSA_BITS
    };
    let drop = significant_bits - precision.max(0);
    let (rounded, shift) = if drop <= 0 {
        (significand, 0)
    } else {
        (
            round_to_nearest_even(significand, sticky, drop as u32),
            drop,
        )
    };
    // The rounded significand holds at most 53 significant bits, so this
    // conversion is exact and the scaling below rounds nothing a second time.
    sign * scale_by_power_of_two(rounded as f64, exponent + shift)
}

/// Drops `bits` low bits, rounding to nearest and to even on a tie. `sticky`
/// says whether digits the parser never kept were non-zero.
fn round_to_nearest_even(significand: u128, sticky: bool, bits: u32) -> u128 {
    if bits >= 128 {
        return u128::from(sticky || significand != 0);
    }
    let kept = significand >> bits;
    let half = 1_u128 << (bits - 1);
    let remainder = significand & ((1_u128 << bits) - 1);
    let below_half = remainder < half;
    let above_half = remainder > half || (remainder == half && sticky);
    if below_half {
        kept
    } else if above_half {
        kept + 1
    } else if kept & 1 == 1 {
        // An exact tie goes to the even neighbour.
        kept + 1
    } else {
        kept
    }
}

/// `value * 2^exponent`, in the ladder `scalbn` uses: the first steps are exact,
/// so at most the last multiplication rounds.
fn scale_by_power_of_two(value: f64, exponent: i64) -> f64 {
    const LARGE: f64 = 8.988_465_674_311_58e307; // 2^1023
    const SMALL: f64 = 2.004_168_360_008_973e-292; // 2^-1022 * 2^53

    let mut value = value;
    let mut exponent = exponent.clamp(-(1 << 20), 1 << 20) as i32;
    if exponent > 1023 {
        value *= LARGE;
        exponent -= 1023;
        if exponent > 1023 {
            value *= LARGE;
            exponent -= 1023;
            if exponent > 1023 {
                exponent = 1023;
            }
        }
    } else if exponent < -1022 {
        value *= SMALL;
        exponent += 1022 - 53;
        if exponent < -1022 {
            value *= SMALL;
            exponent += 1022 - 53;
            if exponent < -1022 {
                exponent = -1022;
            }
        }
    }
    value * f64::from_bits(((0x3ff + i64::from(exponent)) as u64) << 52)
}
