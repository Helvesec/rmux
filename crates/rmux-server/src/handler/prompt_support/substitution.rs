pub(in super::super) fn substitute_prompt_template(template: &str, responses: &[String]) -> String {
    responses
        .iter()
        .enumerate()
        .fold(template.to_owned(), |current, (index, value)| {
            substitute_prompt_value(&current, value, index + 1)
        })
}

fn substitute_prompt_value(template: &str, value: &str, index: usize) -> String {
    if !template.contains('%') {
        return template.to_owned();
    }

    let mut output = String::new();
    let mut chars = template.chars().peekable();
    let mut replaced_first_percent = false;

    while let Some(ch) = chars.next() {
        if ch != '%' {
            output.push(ch);
            continue;
        }

        let next = chars.peek().copied();
        let matches_index = next
            .and_then(|ch| ch.to_digit(10))
            .is_some_and(|digit| digit as usize == index);
        let matches_percent = next == Some('%') && !replaced_first_percent;
        if !matches_index && !matches_percent {
            output.push('%');
            continue;
        }

        if matches_percent {
            replaced_first_percent = true;
        }
        chars.next(); // consume the matched char after %

        let quoted = chars.peek() == Some(&'%');
        if quoted {
            chars.next();
        }

        for vch in value.chars() {
            if quoted && matches!(vch, '"' | '\\' | '$' | ';' | '~') {
                output.push('\\');
            }
            output.push(vch);
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitution_matches_tmux_percent_rules() {
        assert_eq!(
            substitute_prompt_template("rename %% to %1", &[String::from("alpha")]),
            "rename alpha to alpha"
        );
        assert_eq!(
            substitute_prompt_template("%%%1", &[String::from("\"$;~\\")]),
            "\\\"\\$\\;\\~\\\\1"
        );
        assert_eq!(
            substitute_prompt_template("%% %% %2", &[String::from("one"), String::from("two")]),
            "one two two"
        );
    }

    #[test]
    fn substitution_handles_utf8_in_template() {
        assert_eq!(
            substitute_prompt_template("ñoño %% done", &[String::from("val")]),
            "ñoño val done"
        );
        assert_eq!(
            substitute_prompt_template("日本語%1end", &[String::from("x")]),
            "日本語xend"
        );
    }

    #[test]
    fn substitution_no_percent_returns_original() {
        assert_eq!(
            substitute_prompt_template("no percent here", &[String::from("x")]),
            "no percent here"
        );
    }

    #[test]
    fn substitution_trailing_percent_is_literal() {
        assert_eq!(
            substitute_prompt_template("end%", &[String::from("x")]),
            "end%"
        );
    }

    #[test]
    fn substitution_percent_with_out_of_range_index_is_literal() {
        assert_eq!(
            substitute_prompt_template("%3", &[String::from("only-one")]),
            "%3"
        );
    }

    #[test]
    fn substitution_empty_responses() {
        assert_eq!(substitute_prompt_template("static%%", &[]), "static%%");
    }

    #[test]
    fn substitution_percent_only_first_double_percent_replaced_per_response() {
        assert_eq!(substitute_prompt_value("%% and %%", "val", 1), "val and %%");
    }

    #[test]
    fn substitution_quoted_percent_n() {
        assert_eq!(substitute_prompt_value("%1%", "a\"b", 1), "a\\\"b");
    }
}
