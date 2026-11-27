use super::parse_session_line;
use crate::{SessionId, SessionName};

#[test]
fn canonical_inventory_pane_parent_preserves_names() {
    for raw in [
        "ordinary",
        "a\\b",
        "a\\\\b",
        "line\nbreak",
        "café界",
        "bidi\u{202e}name",
    ] {
        let original = SessionName::new(raw).unwrap();
        let parsed = parse_session_line(&format!("{original}\t$7")).unwrap();
        assert_eq!(parsed.name, original);
        assert_eq!(parsed.id, SessionId::new(7));
    }
}

#[test]
fn canonical_inventory_pane_parent_rejects_malformed_rows() {
    for line in [
        "",
        "name",
        "\t$7",
        "name\t7",
        "name\t$bad",
        "name\t$7\textra",
    ] {
        assert!(parse_session_line(line).is_err(), "accepted {line:?}");
    }
}
