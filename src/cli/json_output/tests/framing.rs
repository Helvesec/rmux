use super::super::{
    json_format, list_panes_json_format, list_windows_json_format, parse_delimited_rows,
    parse_escaped_rows, JsonField, LIST_CLIENTS_FIELDS, LIST_PANES_FIELDS, LIST_SESSIONS_FIELDS,
    LIST_WINDOWS_JSON_FIELDS,
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
fn pane_json_frames_each_observation_once() {
    struct MovingPane {
        reads: std::cell::Cell<usize>,
    }

    impl FormatVariables for MovingPane {
        fn format_value(&self, _: FormatVariable) -> Option<String> {
            None
        }

        fn format_value_by_name(&self, name: &str) -> Option<String> {
            if name == "pane_current_path" {
                let reads = self.reads.get();
                self.reads.set(reads + 1);
                return Some(["/", "/usr/share"][reads % 2].to_owned());
            }
            Values {
                fields: LIST_PANES_FIELDS,
                values: &pane_values(""),
            }
            .format_value_by_name(name)
        }
    }

    let pane = MovingPane { reads: 0.into() };
    let payload = format!("{}\n", render_template(&list_panes_json_format(), &pane));
    let rows = parse_escaped_rows(payload.as_bytes(), LIST_PANES_FIELDS, "list-panes")
        .expect("a changing cwd must still produce a complete row");
    assert_eq!(rows[0]["pane_current_path"], "/");
    assert_eq!(pane.reads.get(), 1);
}

#[test]
fn panes_round_trip_utf8_and_all_string_fields() {
    for text in [
        "",
        "unit\x1frow\x1e\n雪🦀",
        "12:ab\n0:\x1e\x1f",
        "\x1fe\x1ff\x1f\n\x1f\x1f\0",
    ] {
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
        let rows = parse_escaped_rows(payload.as_bytes(), LIST_PANES_FIELDS, "list-panes")
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
fn windows_round_trip_multiple_rows_with_embedded_delimiters() {
    let name = "unit\x1frow\x1e\n雪\x1ff\x1fe\x1f\n";
    let variables = Values {
        fields: LIST_WINDOWS_JSON_FIELDS,
        values: &["alpha", "0", "@1", name, "1", "80", "24", "tiled", "1", "0"],
    };
    let row = render_template(&list_windows_json_format(), &variables);
    let payload = format!("{row}\n{row}\n");
    let rows = parse_escaped_rows(payload.as_bytes(), LIST_WINDOWS_JSON_FIELDS, "list-windows")
        .expect("window names survive framing");
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert_eq!(row["window_name"], name);
        assert_eq!(row["window_index"], 0);
        assert_eq!(row["window_active"], true);
    }
}

#[test]
fn escaped_rows_reject_bad_field_counts_invalid_utf8_and_truncation() {
    let fields = [JsonField::string("a"), JsonField::string("b")];
    for payload in [
        &b"missing field\x1f\n"[..],
        b"a\x1ffb\x1ffc\x1f\n",
        b"a\x1ff\xff\x1f\n",
        b"a\x1ffb",
        b"a\x1ffb\x1f",
        b"a\x1ffb\x1f\npartial",
    ] {
        assert!(parse_escaped_rows(payload, &fields, "list-windows").is_err());
    }
    assert!(parse_escaped_rows(b"", &fields, "list-windows")
        .unwrap()
        .is_empty());
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
