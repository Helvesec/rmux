use rmux_core::TargetFindType;

pub(in crate::handler) fn switch_client_target_find_type(value: &str) -> TargetFindType {
    if switch_client_target_is_session_lookup(value) {
        TargetFindType::Session
    } else if value.starts_with('@') && !value.contains('.') {
        TargetFindType::Window
    } else {
        TargetFindType::Pane
    }
}

fn switch_client_target_is_session_lookup(value: &str) -> bool {
    value != "="
        && !value.starts_with('@')
        && !value.starts_with('%')
        && !value.contains(':')
        && !value.contains('.')
}
