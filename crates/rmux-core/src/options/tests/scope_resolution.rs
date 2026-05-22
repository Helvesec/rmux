use super::*;

#[test]
fn global_scope_defaults_follow_the_option_root() {
    for (name, expected) in [
        ("terminal-features", OptionScopeSelector::ServerGlobal),
        ("status", OptionScopeSelector::SessionGlobal),
        ("mode-style", OptionScopeSelector::WindowGlobal),
        (
            "copy-mode-selection-style",
            OptionScopeSelector::WindowGlobal,
        ),
        ("@demo", OptionScopeSelector::SessionGlobal),
    ] {
        assert_eq!(
            default_global_scope_for_option_name(name).expect("option scope resolves"),
            expected,
            "{name} should resolve to its tmux global option tree"
        );
    }
}
