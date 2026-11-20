use super::*;

#[test]
fn empty_user_option_keeps_its_quoted_value_in_named_and_table_output() {
    let mut store = OptionStore::new();
    let scope = OptionScopeSelector::Session(session_name("alpha"));
    store
        .set_by_name(
            scope.clone(),
            "@replay-empty",
            Some(String::new()),
            rmux_proto::SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .expect("explicit empty user value is valid");

    // tmux 3.7b: show-options [-A] -t alpha @replay-empty prints
    // @replay-empty ''; show-options -v prints an empty line. The same
    // named line occurs in the whole table, including the explicit view.
    for mode in [
        ShowOptionsMode::Explicit,
        ShowOptionsMode::Resolved,
        ShowOptionsMode::ResolvedWithInheritanceMarkers,
    ] {
        assert_eq!(
            store
                .show_options_lines_with_mode_filtered(&scope, Some("@replay-empty"), false, mode,)
                .expect("named user option is present"),
            vec!["@replay-empty ''".to_owned()],
            "named mode {mode:?}",
        );
        assert!(store
            .show_options_lines_with_mode(&scope, false, mode)
            .expect("option table is available")
            .contains(&"@replay-empty ''".to_owned()));
        assert_eq!(
            store
                .show_options_lines_with_mode_filtered(&scope, Some("@replay-empty"), true, mode,)
                .expect("value-only user option is present"),
            vec![String::new()],
        );
    }
}
