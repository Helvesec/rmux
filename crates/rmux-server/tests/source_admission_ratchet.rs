//! Source admission regression for the retained-output module split.

#[test]
fn retained_output_modules_preserve_the_path_attribute_ratchet() {
    let cases: &[(&str, &str, &[&str])] = &[
        (
            "crates/rmux-core/src/events/ring.rs",
            include_str!("../../rmux-core/src/events/ring.rs"),
            &[],
        ),
        (
            "crates/rmux-server/src/control.rs",
            include_str!("../../rmux-server/src/control.rs"),
            &[
                concat!("#[", "path = \"control/output_queue.rs\"]"),
                concat!("#[", "path = \"control/pane_output_state.rs\"]"),
                concat!("#[", "path = \"control/transport_drain.rs\"]"),
                concat!("#[", "path = \"control/command_numbering.rs\"]"),
                concat!("#[", "path = \"control/command_validation.rs\"]"),
                concat!("#[", "path = \"control/subscriptions.rs\"]"),
                concat!("#[", "path = \"control/format_subscriptions.rs\"]"),
                concat!("#[", "path = \"control/session_attachment.rs\"]"),
                concat!("#[", "path = \"control/eof_completion.rs\"]"),
                concat!("#[", "path = \"control/tests.rs\"]"),
            ],
        ),
        (
            "crates/rmux-server/src/pane_io/types.rs",
            include_str!("../../rmux-server/src/pane_io/types.rs"),
            &[],
        ),
    ];
    let mut additions = Vec::new();
    for &(path, source, inherited) in cases {
        for (line, attribute) in source.lines().enumerate() {
            let attribute = attribute.trim();
            if attribute.starts_with("#[path") && !inherited.contains(&attribute) {
                additions.push(format!("{path}:{}: {attribute}", line + 1));
            }
        }
    }
    assert!(
        additions.is_empty(),
        "new path attributes violate source admission: {additions:#?}"
    );
}
