use super::command_names;

#[test]
fn fnp1_explicit_creation_and_alias_parse_without_a_split() {
    for name in ["new-pane", "newp"] {
        assert_eq!(
            command_names(&format!("{name} -t %1 -x 30 -y 8 -X -3 -Y 2 -d")),
            ["new-pane"]
        );
    }
    assert_eq!(command_names("split-window -h -t %1"), ["split-window"]);
}
