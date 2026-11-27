#[test]
fn debug_redacts_cli_role_pins_without_changing_parsed_values() {
    let args = super::parse_web_share_args(
        ["--pin-operator", "193847", "--pin-spectator", "826591"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    )
    .expect("valid role PIN arguments");
    assert_eq!(args.pin_operator.as_deref(), Some("193847"));
    assert_eq!(args.pin_spectator.as_deref(), Some("826591"));
    for diagnostic in [format!("{args:?}"), format!("{args:#?}")] {
        for pin in ["193847", "826591"] {
            assert!(!diagnostic.contains(pin), "Debug must redact CLI role PINs");
        }
    }
}
