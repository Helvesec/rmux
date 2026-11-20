use super::{render_runtime_template, RuntimeFormatContext};
use rmux_core::formats::{
    format_variable_descriptor, FormatContext, FormatVariableKey, FormatVariableProvider,
    FormatVariables,
};
use rmux_core::SIXEL_SUPPORT;

/// `RuntimeFormatContext` answers every advertised-but-unresolved name with
/// an empty string, so a build capability has to be resolved before that
/// catch-all or the token silently disappears on every server entry path.
///
/// tmux 3.7b oracle: `#{sixel_support}` is defined for `display-message`
/// with no target and no attached client, and does not move with `-T sixel`
/// or `terminal-features`. The digit is `0` on the oracle and on this
/// build, which has no server-side SIXEL authority to report; an attached
/// client may still list `sixel` in `#{client_termfeatures}`, and that
/// client-scoped sibling is what does move.
#[test]
fn sixel_support_resolves_through_the_runtime_dispatcher_without_context() {
    let expected = "0";
    assert_eq!(SIXEL_SUPPORT, expected == "1");
    let runtime = RuntimeFormatContext::new(FormatContext::new());

    assert_eq!(
        render_runtime_template("#{sixel_support}", &runtime, false),
        expected
    );
    assert_eq!(
        render_runtime_template("#{p5:sixel_support}", &runtime, false),
        format!("{expected}    ")
    );
    // The advertised-name catch-all still governs names this build has no
    // runtime authority for, so the new arm did not widen it.
    assert_eq!(
        runtime.format_value_by_name("alternate_on"),
        Some(String::new())
    );
}

#[test]
fn every_runtime_provider_has_a_dispatch_route() {
    for descriptor in rmux_core::formats::format_variable_descriptors()
        .filter(|descriptor| descriptor.provider() == FormatVariableProvider::Runtime)
    {
        let key = descriptor.key();
        assert!(
            (descriptor.format_variable().is_some()
                && RuntimeFormatContext::covers_runtime_format_value(key))
                || RuntimeFormatContext::covers_pre_base_registered_format_value(key)
                || RuntimeFormatContext::covers_registered_runtime_format_value(key),
            "Runtime provider has no dispatch route: {}",
            descriptor.name()
        );
    }
}

#[test]
fn registered_runtime_dispatch_preserves_empty_and_absent_boundaries() {
    let runtime = RuntimeFormatContext::new(FormatContext::new());

    let empty = format_variable_descriptor("window_active_clients_list")
        .expect("window_active_clients_list descriptor");
    // This lot gives the name a real runtime provider; the empty and
    // supplied answers below are unchanged boundaries and stay as they are.
    assert_eq!(empty.provider(), FormatVariableProvider::Runtime);
    assert_eq!(
        runtime.format_value_by_name(empty.name()),
        Some(String::new())
    );

    let supplied =
        RuntimeFormatContext::new(FormatContext::new().with_named_value(empty.name(), "supplied"));
    assert_eq!(
        supplied.format_value_by_name(empty.name()).as_deref(),
        Some("supplied")
    );

    let wrap = format_variable_descriptor("wrap_flag").expect("wrap descriptor");
    assert_eq!(wrap.key(), FormatVariableKey::WrapFlag);
    assert_eq!(wrap.provider(), FormatVariableProvider::Runtime);
    // tmux answers NULL for this callback when the format tree has no pane
    // (`format.c:3031-3043`), which a registered tmux-table name renders as
    // the empty value - the same shape the four MF1 mode flags already
    // have. The old constant `1` was the bug, so the EXPECTATION moves here
    // and the mask is left alone.
    assert!(wrap.has_known_empty_fallback());
    assert_eq!(
        runtime.format_value_by_name("wrap_flag").as_deref(),
        Some("")
    );

    assert_eq!(
        runtime.format_value_by_name("alternate_on"),
        Some(String::new())
    );

    let pane_created = format_variable_descriptor("pane_created").expect("pane_created descriptor");
    assert_eq!(pane_created.provider(), FormatVariableProvider::Runtime);
    assert!(!pane_created.has_known_empty_fallback());
    assert_eq!(runtime.format_value_by_name("pane_created"), None);

    assert_eq!(runtime.format_value_by_name("definitely_unknown"), None);
}

#[test]
fn render_runtime_template_expands_strftime_tokens_before_formats() {
    let rendered = render_runtime_template("%H:%M", &FormatContext::new(), true);
    let bytes = rendered.as_bytes();

    assert_eq!(bytes.len(), 5);
    assert!(bytes[0].is_ascii_digit());
    assert!(bytes[1].is_ascii_digit());
    assert_eq!(bytes[2], b':');
    assert!(bytes[3].is_ascii_digit());
    assert!(bytes[4].is_ascii_digit());
}

#[test]
fn render_runtime_template_keeps_invalid_percent_literals() {
    assert_eq!(
        render_runtime_template("cpu 100%", &FormatContext::new(), true),
        "cpu 100%"
    );
}
