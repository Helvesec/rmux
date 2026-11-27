//! Hexadecimal operands of `e` that carry a fraction or a binary exponent.
//!
//! The four public witnesses assert the raw bytes the pinned tmux 3.7b printed
//! in one bounded series measured before this correction
//! (`feh1/native/oracle-feh1.json`), each format sent as a single
//! argument to `display-message -p -t A` against an explicitly non-active pane.
//! Every value of a witness is collected before anything is compared, so no
//! case can hide behind an earlier assertion.
//!
//! The edge witnesses that follow belong to the converter this correction adds.
//! Their references are independent of it: bit patterns, `f64::MIN_POSITIVE`,
//! `f64::MAX` and `f64::INFINITY`, never a value this module computed.

use super::*;
use crate::formats::expression::hexadecimal::parse_hexadecimal_float;

fn rendered(formats: &[&'static str]) -> Vec<(&'static str, String)> {
    formats
        .iter()
        .map(|format| (*format, render_template(format, &StaticWindowValues)))
        .collect()
}

#[test]
fn hexadecimal_float_arithmetic_matches_the_measured_oracle() {
    // tmux printed b"4.00\n", b"0.125\n" and b"2.00\n".
    let measured = rendered(&[
        "#{e|+|f|2:0x1.8p+1,1}",
        "#{e|+|f|3:-0X1.CP-1,+0x1p+0}",
        "#{e|+|f|2:0x1.8,0x.8}",
    ]);

    assert_eq!(
        measured,
        vec![
            ("#{e|+|f|2:0x1.8p+1,1}", "4.00".to_owned()),
            ("#{e|+|f|3:-0X1.CP-1,+0x1p+0}", "0.125".to_owned()),
            ("#{e|+|f|2:0x1.8,0x.8}", "2.00".to_owned()),
        ]
    );
}

#[test]
fn the_integer_mode_and_its_precision_keep_their_semantics() {
    // tmux printed b"2.00\n" for the integer division - 5.75 and 2.875 are
    // truncated to 5 and 2 before it - and b"17\n" for the acquired
    // hexadecimal integer, which is a preservation reading, not a new one.
    let measured = rendered(&["#{e|/||2:0x1.7p+2,0x1.7p+1}", "#{e|+|:0x10,1}"]);

    assert_eq!(
        measured,
        vec![
            ("#{e|/||2:0x1.7p+2,0x1.7p+1}", "2.00".to_owned()),
            ("#{e|+|:0x10,1}", "17".to_owned()),
        ]
    );
}

#[test]
fn hexadecimal_comparisons_match_the_measured_oracle() {
    // tmux printed b"1.000\n", b"1.00\n" and b"1.00\n": the integer comparison
    // truncates both operands to 2, the float one compares 3 with 2, and the
    // last one compares a hexadecimal operand with a decimal.
    let measured = rendered(&[
        "#{e|==||3:0x1.7p+1,0x1.1p+1}",
        "#{e|>|f|2:0x1.8p+1,0x1p1}",
        "#{e|==|f|2:0x1p2,4}",
    ]);

    assert_eq!(
        measured,
        vec![
            ("#{e|==||3:0x1.7p+1,0x1.1p+1}", "1.000".to_owned()),
            ("#{e|>|f|2:0x1.8p+1,0x1p1}", "1.00".to_owned()),
            ("#{e|==|f|2:0x1p2,4}", "1.00".to_owned()),
        ]
    );
}

#[test]
fn invalid_hexadecimal_shapes_stay_refused_between_valid_ones() {
    // tmux printed a line feed alone for the three invalid shapes, and the two
    // neighbours keep what they already printed: b"17\n" and b"2.03\n". The
    // valid reading at each end frames the invalid ones in the same witness.
    let measured = rendered(&[
        "#{e|+|f|2:0x1.8p+1,1}",
        "#{e|+|f|2:0x1p,1}",
        "#{e|+|f|2:1,0x1.8p+1z}",
        "#{e|+|f|2:0x.p1,1}",
        "#{e|+|:0x10,1}",
        "#{e|/|f|2:5.9,2.9}",
    ]);

    assert_eq!(
        measured,
        vec![
            ("#{e|+|f|2:0x1.8p+1,1}", "4.00".to_owned()),
            ("#{e|+|f|2:0x1p,1}", String::new()),
            ("#{e|+|f|2:1,0x1.8p+1z}", String::new()),
            ("#{e|+|f|2:0x.p1,1}", String::new()),
            ("#{e|+|:0x10,1}", "17".to_owned()),
            ("#{e|/|f|2:5.9,2.9}", "2.03".to_owned()),
        ]
    );
}

#[test]
fn the_conversion_lands_on_the_value_its_own_exponent_allows() {
    // References that owe nothing to this converter: the bit patterns of the
    // smallest subnormal and of one ulp above it, the smallest normal, the
    // largest finite value and infinity.
    let smallest_subnormal = f64::from_bits(1);
    let two_subnormals = f64::from_bits(2);
    let one_ulp_above_one = f64::from_bits(0x3ff0_0000_0000_0001);

    let measured = vec![
        // The subnormal floor, and the value half a step below it, which ties
        // to the even neighbour: zero.
        ("0x1p-1074", parse_hexadecimal_float("0x1p-1074")),
        ("0x1p-1075", parse_hexadecimal_float("0x1p-1075")),
        // Exactly between the first two subnormals: the tie goes to even.
        ("0x1.8p-1074", parse_hexadecimal_float("0x1.8p-1074")),
        // The normal boundary, written from underneath.
        ("0x1p-1022", parse_hexadecimal_float("0x1p-1022")),
        ("0x0.8p-1021", parse_hexadecimal_float("0x0.8p-1021")),
        // The largest finite value, then one binary decade past the top.
        (
            "0x1.fffffffffffffp+1023",
            parse_hexadecimal_float("0x1.fffffffffffffp+1023"),
        ),
        ("0x1p+1024", parse_hexadecimal_float("0x1p+1024")),
        // An exponent far past anything representable still answers in the
        // time the text takes to read.
        (
            "0x1p+99999999999999",
            parse_hexadecimal_float("0x1p+99999999999999"),
        ),
        // A zero significand is a zero at any exponent, and it keeps its sign.
        (
            "0x0p+99999999999999",
            parse_hexadecimal_float("0x0p+99999999999999"),
        ),
        ("-0x0p+0", parse_hexadecimal_float("-0x0p+0")),
        // A tie at the last bit of one, and the same tie broken by a digit far
        // to the right: only the second one carries.
        (
            "0x1.00000000000008p+0",
            parse_hexadecimal_float("0x1.00000000000008p+0"),
        ),
        (
            "0x1.000000000000080000000001p+0",
            parse_hexadecimal_float("0x1.000000000000080000000001p+0"),
        ),
        // A carry that runs the whole significand up to the next power of two.
        (
            "0x1.fffffffffffff8p+0",
            parse_hexadecimal_float("0x1.fffffffffffff8p+0"),
        ),
    ];

    assert_eq!(
        measured,
        vec![
            ("0x1p-1074", Some(smallest_subnormal)),
            ("0x1p-1075", Some(0.0)),
            ("0x1.8p-1074", Some(two_subnormals)),
            ("0x1p-1022", Some(f64::MIN_POSITIVE)),
            ("0x0.8p-1021", Some(f64::MIN_POSITIVE)),
            ("0x1.fffffffffffffp+1023", Some(f64::MAX)),
            ("0x1p+1024", Some(f64::INFINITY)),
            ("0x1p+99999999999999", Some(f64::INFINITY)),
            ("0x0p+99999999999999", Some(0.0)),
            ("-0x0p+0", Some(0.0)),
            ("0x1.00000000000008p+0", Some(1.0)),
            ("0x1.000000000000080000000001p+0", Some(one_ulp_above_one)),
            ("0x1.fffffffffffff8p+0", Some(2.0)),
        ]
    );
    // The signed zero keeps its sign, which the equality above cannot show.
    assert!(parse_hexadecimal_float("-0x0p+0")
        .expect("a signed zero converts")
        .is_sign_negative());
    assert!(parse_hexadecimal_float("0x0p+0")
        .expect("a zero converts")
        .is_sign_positive());
}

#[test]
fn the_converter_answers_none_outside_its_own_domain() {
    let measured: Vec<(&str, Option<f64>)> = [
        // Shapes that are this domain but malformed.
        "0x1p",
        "0x1.8p+",
        "0xp1",
        "0x.p1",
        "0x1.2.3p1",
        "0x1.8p+1z",
        "0x1.8p1.5",
        // Shapes that belong to the paths that already existed: a plain
        // hexadecimal integer, a decimal, and a prefix with nothing after it.
        "0x10",
        "10",
        "0x",
    ]
    .into_iter()
    .map(|value| (value, parse_hexadecimal_float(value)))
    .collect();

    assert!(
        measured.iter().all(|(_, value)| value.is_none()),
        "{measured:?}"
    );
}
