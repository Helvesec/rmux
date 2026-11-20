use super::super::{
    json_format, length_prefixed_filter_format, list_panes_json_format, parse_delimited_rows,
    parse_length_prefixed_rows, parse_length_prefixed_rows_with_filter, read_length_prefixed_value,
    JsonField, LIST_CLIENTS_FIELDS, LIST_PANES_FIELDS, LIST_SESSIONS_FIELDS,
};
use rmux_core::formats::{render_template, FormatVariable, FormatVariables};
use serde_json::json;

struct Values<'a> {
    fields: &'a [JsonField],
    values: &'a [&'a str],
}

impl FormatVariables for Values<'_> {
    fn format_value(&self, _: FormatVariable) -> Option<String> {
        None
    }

    fn format_value_by_name(&self, name: &str) -> Option<String> {
        self.fields
            .iter()
            .position(|field| field.name == name)
            .map(|index| self.values[index].to_owned())
    }
}

fn pane_values(path: &str) -> [&str; 10] {
    ["alpha", "0", "1", "%2", "80", "24", "1", "0", "sh", path]
}

#[test]
fn panes_round_trip_utf8_byte_lengths_and_all_string_fields() {
    for text in ["", "unit\x1frow\x1e\n雪🦀", "12:ab\n0:\x1e\x1f"] {
        let mut values = pane_values(text);
        values[8] = text;
        let variables = Values {
            fields: LIST_PANES_FIELDS,
            values: &values,
        };
        let payload = format!(
            "{}\n",
            render_template(&list_panes_json_format(), &variables)
        );
        let rows = parse_length_prefixed_rows(payload.as_bytes(), LIST_PANES_FIELDS, "list-panes")
            .expect("pane values survive framing");
        assert_eq!(
            rows,
            vec![json!({
                "session_name": "alpha", "window_index": 0, "pane_index": 1,
                "pane_id": "%2", "pane_width": 80, "pane_height": 24,
                "pane_active": true, "pane_dead": false,
                "pane_current_command": text, "pane_current_path": text,
            })]
        );
    }
}

#[test]
fn filter_length_prefix_preserves_expanded_values_and_template_literals() {
    let values = pane_values("/work/unit\x1frow\x1e\n雪");
    let variables = Values {
        fields: LIST_PANES_FIELDS,
        values: &values,
    };
    for (filter, expected) in [
        ("", ""),
        ("0", "0"),
        ("1", "1"),
        ("value\x1f\x1e\n雪", "value\x1f\x1e\n雪"),
        ("#{pane_current_path}", values[9]),
        ("#{?pane_active,yes,no}", "yes"),
        ("literal},:#", "literal},:"),
        ("#{l:foo#}bar}", "foo}bar"),
        ("##}#{pane_index}#}", "#}1}"),
        ("%Y雪", "%Y雪"),
    ] {
        let format = length_prefixed_filter_format(filter, &list_panes_json_format());
        let payload = format!("{}\n", render_template(&format, &variables));
        assert_eq!(render_template(filter, &variables), expected);
        let value = read_length_prefixed_value(payload.as_bytes(), &mut 0, "list-panes", "filter")
            .expect("filter decodes");
        assert_eq!(value, expected, "filter {filter:?}");
        assert!(payload.starts_with(&format!("{}:{expected}", expected.len())));
        let rows = parse_length_prefixed_rows_with_filter(
            payload.as_bytes(),
            LIST_PANES_FIELDS,
            "list-panes",
            true,
        )
        .expect("filtered row decodes");
        assert_eq!(rows.len(), usize::from(!matches!(expected, "" | "0")));
    }
}

#[test]
fn filtered_rows_keep_boundaries_after_empty_false_and_true_values() {
    let values = pane_values("/work/\x1f\x1e\n雪");
    let variables = Values {
        fields: LIST_PANES_FIELDS,
        values: &values,
    };
    let mut payload = String::new();
    for filter in ["", "1", "0", "#{pane_current_path}"] {
        let format = length_prefixed_filter_format(filter, &list_panes_json_format());
        payload.push_str(&render_template(&format, &variables));
        payload.push('\n');
    }
    let rows = parse_length_prefixed_rows_with_filter(
        payload.as_bytes(),
        LIST_PANES_FIELDS,
        "list-panes",
        true,
    )
    .expect("mixed rows decode");
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row["pane_current_path"] == values[9]));
    for filtered in [false, true] {
        assert!(parse_length_prefixed_rows_with_filter(
            b"",
            LIST_PANES_FIELDS,
            "list-panes",
            filtered,
        )
        .unwrap()
        .is_empty());
    }
}

#[test]
fn length_prefix_decoder_rejects_truncation_invalid_utf8_and_missing_terminators() {
    let fields = [JsonField::string("value")];
    for payload in [
        &b":x\n"[..],
        b"x:x\n",
        b"2:x",
        b"1:\xff\n",
        b"1:\xe9\x9b\xaa\n",
        b"1:x",
        b"18446744073709551615:x\n",
        b"1:x\n1:",
    ] {
        assert!(parse_length_prefixed_rows(payload, &fields, "list-windows").is_err());
    }
}

#[test]
fn clients_and_sessions_keep_their_existing_framing_and_types() {
    let clients = [
        "client", "alpha", "/dev/tty", "80", "24", "123", "0", "1", "1",
    ];
    let sessions = ["$1", "alpha", "2", "1", "0", "", "123"];
    for (fields, values, command) in [
        (LIST_CLIENTS_FIELDS, clients.as_slice(), "list-clients"),
        (LIST_SESSIONS_FIELDS, sessions.as_slice(), "list-sessions"),
    ] {
        let variables = Values { fields, values };
        let payload = format!("{}\n", render_template(&json_format(fields), &variables));
        let rows = parse_delimited_rows(&payload, fields, command).expect("sibling row decodes");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].as_object().unwrap().len(), fields.len());
        assert!(parse_delimited_rows("", fields, command)
            .unwrap()
            .is_empty());
    }
}
