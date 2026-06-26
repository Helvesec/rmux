//! Client-side terminal capability detection shared by the full and tiny CLIs.

use rmux_proto::ClientTerminalContext;

pub(crate) fn client_terminal_context_from_parts(
    terminal_features: Vec<String>,
    utf8: bool,
) -> ClientTerminalContext {
    let mut context = ClientTerminalContext {
        terminal_features,
        utf8,
    };
    apply_detected_client_terminal_features(&mut context);
    context
}

pub(crate) fn apply_detected_client_terminal_features(context: &mut ClientTerminalContext) {
    #[cfg(windows)]
    if std::env::var_os("WT_SESSION").is_some_and(|value| !value.is_empty()) {
        apply_windows_terminal_features(context);
    }
    #[cfg(not(windows))]
    let _ = context;
}

#[cfg(windows)]
fn apply_windows_terminal_features(context: &mut ClientTerminalContext) {
    context.utf8 = true;
    push_unique_terminal_feature(&mut context.terminal_features, "sync");
    push_unique_terminal_feature(&mut context.terminal_features, "bpaste");
    push_unique_terminal_feature(&mut context.terminal_features, "mouse");
}

#[cfg(windows)]
fn push_unique_terminal_feature(features: &mut Vec<String>, feature: &str) {
    if !features
        .iter()
        .any(|value| value.eq_ignore_ascii_case(feature))
    {
        features.push(feature.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::client_terminal_context_from_parts;

    #[test]
    fn detected_client_terminal_context_preserves_explicit_features() {
        let context = client_terminal_context_from_parts(vec!["RGB".to_owned()], true);

        assert!(context.utf8);
        assert!(context
            .terminal_features
            .iter()
            .any(|feature| feature == "RGB"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_terminal_features_are_sent_by_client_context() {
        let mut context = rmux_proto::ClientTerminalContext::default();

        super::apply_windows_terminal_features(&mut context);

        assert!(context.utf8);
        assert_eq!(context.terminal_features, vec!["sync", "bpaste", "mouse"]);
    }

    #[cfg(windows)]
    #[test]
    fn detected_windows_terminal_features_are_not_duplicated() {
        let mut context = rmux_proto::ClientTerminalContext {
            terminal_features: vec!["SYNC".to_owned(), "BPASTE".to_owned(), "MOUSE".to_owned()],
            utf8: false,
        };

        super::apply_windows_terminal_features(&mut context);

        assert!(context.utf8);
        assert_eq!(context.terminal_features, vec!["SYNC", "BPASTE", "MOUSE"]);
    }
}
