use chrono::{DateTime, Datelike, Local, LocalResult, TimeZone};

pub(super) fn format_time_string(
    value: &str,
    pretty: bool,
    format: Option<&str>,
) -> Option<String> {
    let epoch = value.trim().parse::<i64>().ok()?;
    if epoch <= 0 {
        return None;
    }
    let date_time = local_datetime(epoch)?;

    if pretty {
        return Some(format_pretty_time(date_time));
    }

    Some(match format {
        Some(format) => format_strftime(&date_time, format),
        None => format_strftime(&date_time, "%a %b %e %H:%M:%S %Y"),
    })
}

fn format_pretty_time(time: DateTime<Local>) -> String {
    let now = Local::now();
    let age = now.timestamp().saturating_sub(time.timestamp());

    if age < 24 * 60 * 60 {
        return time.format("%H:%M").to_string();
    }

    if (time.year() == now.year() && time.month() == now.month()) || age < 28 * 24 * 60 * 60 {
        return time.format("%a%d").to_string();
    }

    let same_or_previous_year = (time.year() == now.year() && time.month() < now.month())
        || (time.year() == now.year() - 1 && time.month() > now.month());
    if same_or_previous_year {
        return time.format("%d%b").to_string();
    }

    time.format("%b%y").to_string()
}

fn local_datetime(epoch: i64) -> Option<DateTime<Local>> {
    match Local.timestamp_opt(epoch, 0) {
        LocalResult::Single(date_time) => Some(date_time),
        LocalResult::Ambiguous(date_time, _) => Some(date_time),
        LocalResult::None => None,
    }
}

/// Expands strftime tokens in a tmux format template without panicking on invalid literals.
pub fn expand_time_tokens(template: &str) -> String {
    format_strftime(&Local::now(), template)
}

fn format_strftime(time: &DateTime<Local>, template: &str) -> String {
    let escaped = escape_bare_percent_literals(template);
    time.format(&escaped).to_string()
}

fn escape_bare_percent_literals(template: &str) -> String {
    let mut output = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            output.push(ch);
            continue;
        }
        output.push_str(&escaped_percent_sequence(&mut chars));
    }
    output
}

fn escaped_percent_sequence(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    match chars.peek().copied() {
        Some('%') => {
            let _ = chars.next();
            "%%".to_owned()
        }
        Some(next) if is_supported_strftime_code(next) => {
            let _ = chars.next();
            format!("%{next}")
        }
        Some('-' | '_' | '0') => {
            let modifier = chars.next().expect("peeked modifier exists");
            match chars.peek().copied() {
                Some(code) if is_supported_modifier_code(code) => {
                    let _ = chars.next();
                    format!("%{modifier}{code}")
                }
                _ => format!("%%{modifier}"),
            }
        }
        Some(':') => {
            let mut spec = String::from("%");
            let mut colon_count = 0;
            while chars.peek().is_some_and(|ch| *ch == ':') {
                spec.push(':');
                colon_count += 1;
                let _ = chars.next();
            }
            match chars.peek().copied() {
                Some('z') => {
                    let _ = chars.next();
                    spec.push('z');
                    if colon_count <= 3 {
                        spec
                    } else {
                        escape_literal_percent_spec(&spec)
                    }
                }
                _ => escape_literal_percent_spec(&spec),
            }
        }
        Some('#') => {
            let _ = chars.next();
            match chars.peek().copied() {
                Some('z') => {
                    let _ = chars.next();
                    "%%#z".to_owned()
                }
                _ => "%%#".to_owned(),
            }
        }
        Some('.') => {
            let mut spec = String::from("%.");
            let mut digits = String::new();
            let _ = chars.next();
            while let Some(digit) = chars.peek().copied().filter(char::is_ascii_digit) {
                spec.push(digit);
                digits.push(digit);
                let _ = chars.next();
            }
            match chars.peek().copied() {
                Some('f') => {
                    let _ = chars.next();
                    spec.push('f');
                    if digits.is_empty() || is_valid_fraction_precision(&digits) {
                        spec
                    } else {
                        escape_literal_percent_spec(&spec)
                    }
                }
                _ => escape_literal_percent_spec(&spec),
            }
        }
        Some(digit) if digit.is_ascii_digit() => {
            let mut spec = String::from("%");
            let mut digits = String::new();
            while let Some(digit) = chars.peek().copied().filter(char::is_ascii_digit) {
                spec.push(digit);
                digits.push(digit);
                let _ = chars.next();
            }
            match chars.peek().copied() {
                Some('f') => {
                    let _ = chars.next();
                    spec.push('f');
                    if is_valid_fraction_precision(&digits) {
                        spec
                    } else {
                        escape_literal_percent_spec(&spec)
                    }
                }
                _ => escape_literal_percent_spec(&spec),
            }
        }
        _ => "%%".to_owned(),
    }
}

fn escape_literal_percent_spec(spec: &str) -> String {
    format!("%{spec}")
}

fn is_valid_fraction_precision(digits: &str) -> bool {
    matches!(digits, "3" | "6" | "9")
}

fn is_supported_strftime_code(ch: char) -> bool {
    matches!(
        ch,
        'A' | 'a'
            | 'B'
            | 'b'
            | 'C'
            | 'c'
            | 'D'
            | 'd'
            | 'e'
            | 'F'
            | 'G'
            | 'g'
            | 'H'
            | 'h'
            | 'I'
            | 'j'
            | 'k'
            | 'l'
            | 'M'
            | 'm'
            | 'n'
            | 'P'
            | 'p'
            | 'R'
            | 'r'
            | 'S'
            | 's'
            | 'T'
            | 't'
            | 'U'
            | 'u'
            | 'V'
            | 'v'
            | 'W'
            | 'w'
            | 'X'
            | 'x'
            | 'Y'
            | 'y'
            | 'Z'
            | 'z'
            | '+'
            | 'f'
    )
}

fn is_supported_modifier_code(ch: char) -> bool {
    matches!(
        ch,
        'C' | 'd'
            | 'e'
            | 'G'
            | 'g'
            | 'H'
            | 'I'
            | 'j'
            | 'k'
            | 'l'
            | 'M'
            | 'm'
            | 'S'
            | 's'
            | 'U'
            | 'u'
            | 'V'
            | 'W'
            | 'w'
            | 'Y'
            | 'y'
            | 'f'
    )
}
