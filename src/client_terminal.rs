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
    apply_windows_terminal_features(
        context,
        std::env::var_os("WT_SESSION").is_some_and(|value| !value.is_empty()),
    );
    #[cfg(not(windows))]
    let _ = context;
}

#[cfg(windows)]
fn apply_windows_terminal_features(context: &mut ClientTerminalContext, is_windows_terminal: bool) {
    // rmux always drives the outer terminal as VT on Windows — a console outer
    // runs with ENABLE_VIRTUAL_TERMINAL_PROCESSING and a pipe outer is raw VT —
    // so advertise the base VT feature set for any outer, not only Windows
    // Terminal. Without this a VT outer reached without WT_SESSION
    // (OpenSSH-into-Windows, WezTerm, Alacritty, VS Code, mintty) never has
    // mouse reporting or bracketed paste enabled on it (issue #93).
    push_unique_terminal_feature(&mut context.terminal_features, "sync");
    push_unique_terminal_feature(&mut context.terminal_features, "bpaste");
    push_unique_terminal_feature(&mut context.terminal_features, "mouse");
    // Advertise clipboard (OSC 52) too: Windows Terminal sets the system
    // clipboard from OSC 52 natively, and any VT outer that does not simply
    // ignores the sequence. Without it the daemon has no Ms template, so a
    // pane's OSC 52 under `set-clipboard on` never reaches the outer (issue
    // #91). Whether inbound OSC 52 is actually relayed still depends on
    // set-clipboard being `on`, which is enforced daemon-side.
    push_unique_terminal_feature(&mut context.terminal_features, "clipboard");
    // utf8 stays gated: Windows Terminal is known UTF-8, while other outers
    // are inferred from the console code page / locale elsewhere.
    if is_windows_terminal {
        context.utf8 = true;
    }
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
    fn windows_terminal_advertises_vt_features_and_utf8() {
        let mut context = rmux_proto::ClientTerminalContext::default();

        super::apply_windows_terminal_features(&mut context, true);

        assert!(context.utf8);
        assert_eq!(
            context.terminal_features,
            vec!["sync", "bpaste", "mouse", "clipboard"]
        );
    }

    #[cfg(windows)]
    #[test]
    fn non_windows_terminal_vt_outer_still_advertises_mouse_and_bpaste() {
        // Issue #93: a VT outer reached without WT_SESSION (SSH/WezTerm/…) must
        // still advertise mouse + bracketed paste so the daemon enables them on
        // the outer. utf8 is NOT forced here (only Windows Terminal is known
        // UTF-8); the console code page decides it elsewhere.
        let mut context = rmux_proto::ClientTerminalContext::default();

        super::apply_windows_terminal_features(&mut context, false);

        assert!(!context.utf8);
        assert_eq!(
            context.terminal_features,
            vec!["sync", "bpaste", "mouse", "clipboard"]
        );
    }

    #[cfg(windows)]
    #[test]
    fn detected_windows_terminal_features_are_not_duplicated() {
        let mut context = rmux_proto::ClientTerminalContext {
            terminal_features: vec!["SYNC".to_owned(), "BPASTE".to_owned(), "MOUSE".to_owned()],
            utf8: false,
        };

        super::apply_windows_terminal_features(&mut context, true);

        assert!(context.utf8);
        assert_eq!(
            context.terminal_features,
            vec!["SYNC", "BPASTE", "MOUSE", "clipboard"]
        );
    }
}
