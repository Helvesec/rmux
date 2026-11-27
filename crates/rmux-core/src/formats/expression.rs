use super::{format_choose, ExpandState, FormatModifier, FormatVariables};

// The witnesses of this domain reach the converter directly for the edge cases
// whose references are bit patterns rather than rendered text.
pub(in crate::formats) mod hexadecimal;

pub(super) fn format_expression<V>(
    state: &mut ExpandState,
    body: &str,
    modifier: &FormatModifier,
    variables: &V,
) -> String
where
    V: FormatVariables + ?Sized,
{
    let Some(operator) = modifier.argv.first().map(String::as_str) else {
        return String::new();
    };
    let Some((left, right)) = format_choose(state, body, variables) else {
        return String::new();
    };

    let expression = expression_formatting(modifier);
    // A precision the source refuses fails the whole expression, whether or not
    // the calculation itself is a floating one (format.c:4958-4965).
    let precision = match expression.precision {
        ExpressionPrecision::Invalid => return String::new(),
        precision => precision,
    };

    if is_comparison_operator(operator) {
        let mode = if expression.float {
            ExpressionComparisonMode::Float
        } else {
            ExpressionComparisonMode::Integer
        };
        let Some(value) = numeric_compare(operator, &left, &right, mode) else {
            return String::new();
        };
        return match precision {
            ExpressionPrecision::Valid(precision) => {
                format_float_value(if value { 1.0 } else { 0.0 }, precision)
            }
            _ => bool_string(value),
        };
    }

    if expression.float {
        let Some(value) = numeric_operation(operator, &left, &right) else {
            return String::new();
        };
        return match precision {
            ExpressionPrecision::Valid(precision) => format_float_value(value, precision),
            // `f` always carries its own default precision, so this arm only
            // exists to keep the match total.
            _ => format_float_value(value, DEFAULT_FLOAT_PRECISION),
        };
    }

    let Some(value) = integer_operation(operator, &left, &right) else {
        return String::new();
    };
    match precision {
        ExpressionPrecision::Valid(precision) => integer_value_with_precision(&value, precision),
        _ => value,
    }
}

fn numeric_operation(operator: &str, left: &str, right: &str) -> Option<f64> {
    let left = parse_number(left)?;
    let right = parse_number(right)?;
    Some(match operator {
        "+" => left + right,
        "-" => left - right,
        "*" => left * right,
        "/" => left / right,
        "m" | "%" | "%%" => {
            if right == 0.0 {
                f64::NAN
            } else {
                left % right
            }
        }
        _ => return None,
    })
}

fn integer_operation(operator: &str, left: &str, right: &str) -> Option<String> {
    if !matches!(operator, "+" | "-" | "*" | "/" | "m" | "%" | "%%") {
        return None;
    }

    let left = arithmetic_operand(left)?;
    let right = arithmetic_operand(right)?;
    let value = match operator {
        "+" => left + right,
        "-" => left - right,
        "*" => left * right,
        "/" => left / right,
        // Both spellings reach the operator on glibc; on the darwin oracle
        // BSD strftime consumes a lone '%' upstream and doubles '%%' into
        // '%', so accepting both matches the Linux oracle end-to-end and the
        // darwin oracle's post-strftime behavior.
        "m" | "%" | "%%" => left % right,
        _ => return None,
    };
    Some(integer_result(value))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpressionComparisonMode {
    Integer,
    Float,
}

fn numeric_compare(
    operator: &str,
    left: &str,
    right: &str,
    mode: ExpressionComparisonMode,
) -> Option<bool> {
    let parse_operand = match mode {
        ExpressionComparisonMode::Integer => comparison_operand,
        ExpressionComparisonMode::Float => parse_number,
    };
    let left = parse_operand(left)?;
    let right = parse_operand(right)?;
    Some(match operator {
        "==" if mode == ExpressionComparisonMode::Float => (left - right).abs() < 1e-9,
        "!=" if mode == ExpressionComparisonMode::Float => (left - right).abs() > 1e-9,
        "==" => left == right,
        "!=" => left != right,
        ">" => left > right,
        ">=" => left >= right,
        "<" => left < right,
        "<=" => left <= right,
        _ => return None,
    })
}

fn arithmetic_operand(value: &str) -> Option<f64> {
    let value = parse_number(value)?;
    if value.is_finite() && value > i64::MIN as f64 && value < i64::MAX as f64 {
        return Some((value as i64) as f64);
    }
    Some(i64::MIN as f64)
}

fn comparison_operand(value: &str) -> Option<f64> {
    arithmetic_operand(value)
}

fn parse_number(value: &str) -> Option<f64> {
    // Preserve the product's tolerance of surrounding C whitespace, while
    // rejecting Unicode whitespace that strtod does not consume.
    let value = value.trim_matches(|ch| matches!(ch, ' ' | '\t'..='\r'));
    if value.is_empty() {
        return Some(0.0);
    }
    if value.eq_ignore_ascii_case("nan") {
        return Some(f64::NAN);
    }
    if value.eq_ignore_ascii_case("inf") || value.eq_ignore_ascii_case("+inf") {
        return Some(f64::INFINITY);
    }
    if value.eq_ignore_ascii_case("-inf") {
        return Some(f64::NEG_INFINITY);
    }
    value
        .parse::<f64>()
        .ok()
        .or_else(|| parse_prefixed_integer(value).map(|integer| integer as f64))
        // Only the forms the two paths above cannot read reach the hexadecimal
        // significand converter: it answers None for everything else, so the
        // decimal path, the prefixed integers and their clamps stay as they are.
        .or_else(|| hexadecimal::parse_hexadecimal_float(value))
}

fn parse_prefixed_integer(value: &str) -> Option<i64> {
    let (negative, digits) = match value.as_bytes().first().copied() {
        Some(b'-') => (true, &value[1..]),
        Some(b'+') => (false, &value[1..]),
        _ => (false, value),
    };
    let (base, digits) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
        .map(|digits| (16, digits))?;
    if digits.is_empty() {
        return None;
    }
    let unsigned = i128::from_str_radix(digits, base).ok()?;
    let signed = if negative { -unsigned } else { unsigned };
    Some(signed.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64)
}

fn integer_result(value: f64) -> String {
    if value.is_nan()
        || value == f64::INFINITY
        || value == f64::NEG_INFINITY
        || value >= i64::MAX as f64
        || value < i64::MIN as f64
    {
        return i64::MIN.to_string();
    }
    (value as i64).to_string()
}

fn is_comparison_operator(operator: &str) -> bool {
    matches!(operator, "==" | "!=" | ">" | ">=" | "<" | "<=")
}

const MIN_EXPRESSION_FLOAT_PRECISION: i64 = -100;
const MAX_EXPRESSION_FLOAT_PRECISION: i64 = 100;
const DEFAULT_NEGATIVE_FLOAT_PRECISION: usize = 6;
const DEFAULT_FLOAT_PRECISION: usize = 2;

/// The explicit third argument of `e`, which the source validates on its own,
/// whether or not the second one asked for a floating calculation
/// (format.c:4951-4965).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpressionPrecision {
    Absent,
    Valid(usize),
    Invalid,
}

/// The two independent choices an `e` modifier carries: `f` decides how the
/// operands and the result are computed, the third argument decides how many
/// decimals the result is printed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExpressionFormatting {
    float: bool,
    precision: ExpressionPrecision,
}

fn expression_formatting(modifier: &FormatModifier) -> ExpressionFormatting {
    let options = modifier.argv.get(1).map(String::as_str).unwrap_or_default();
    let float = options.contains('f');
    let Some(raw_precision) = modifier.argv.get(2) else {
        // No third argument at all: `f` brings its own default precision, and
        // without it the integer result is left as it is. A third argument that
        // is present but empty is a different thing entirely - the source
        // validates it as soon as there are three of them (format.c:4958-4965)
        // and an empty conversion is invalid - so it must not land here.
        return ExpressionFormatting {
            float,
            precision: if float {
                ExpressionPrecision::Valid(DEFAULT_FLOAT_PRECISION)
            } else {
                ExpressionPrecision::Absent
            },
        };
    };
    ExpressionFormatting {
        float,
        precision: parse_expression_precision(raw_precision),
    }
}

fn parse_expression_precision(raw_precision: &str) -> ExpressionPrecision {
    // strtonum accepts an initial run of C whitespace, but no trailing bytes.
    let raw_precision = raw_precision.trim_start_matches(|ch| matches!(ch, ' ' | '\t'..='\r'));
    let Ok(precision) = raw_precision.parse::<i64>() else {
        return ExpressionPrecision::Invalid;
    };
    if precision < MIN_EXPRESSION_FLOAT_PRECISION {
        return ExpressionPrecision::Invalid;
    }
    if precision < 0 {
        return ExpressionPrecision::Valid(DEFAULT_NEGATIVE_FLOAT_PRECISION);
    }
    if precision > MAX_EXPRESSION_FLOAT_PRECISION {
        return ExpressionPrecision::Invalid;
    }
    ExpressionPrecision::Valid(precision as usize)
}

/// Prints an integer result with an explicit precision. The source keeps the
/// integer it computed and prints it as a double (format.c:5030), so the
/// truncation of the operands and of the result stays exactly where it was.
fn integer_value_with_precision(value: &str, precision: usize) -> String {
    match value.parse::<i64>() {
        Ok(integer) => format_float_value(integer as f64, precision),
        Err(_) => value.to_owned(),
    }
}

fn bool_string(value: bool) -> String {
    if value {
        "1".to_owned()
    } else {
        "0".to_owned()
    }
}

fn format_float_value(value: f64, precision: usize) -> String {
    if value.is_nan() {
        // All NaN results render as "-nan": the Linux x86_64 deployment
        // oracle prints "-nan" (glibc + SSE quiet-NaN sign), and normalizing
        // every NaN producer (modulo-by-zero, inf-inf, 0/0, ...) keeps RMUX
        // output identical across CPUs, unlike raw hardware NaN signs.
        return "-nan".to_owned();
    }
    format!("{value:.precision$}")
}
