use super::*;
use crate::formats::DEFAULT_LIST_WINDOWS_ALL_FORMAT;

#[test]
fn default_list_windows_format_matches_canonical_output() {
    let rendered = render_template(DEFAULT_LIST_WINDOWS_FORMAT, &StaticWindowValues);

    assert_eq!(
        rendered,
        format!(
            "5: logs* (3 panes) [120x40] [layout {}] @9 (active)",
            sample_window_layout()
        )
    );
}

#[test]
fn default_list_windows_all_format_matches_canonical_output() {
    let rendered = render_template(DEFAULT_LIST_WINDOWS_ALL_FORMAT, &StaticWindowValues);

    assert_eq!(rendered, "alpha:5: logs* (3 panes) [120x40] ");
}

#[test]
fn default_display_message_format_uses_only_supported_variables() {
    assert_eq!(
        render_template(super::DEFAULT_DISPLAY_MESSAGE_FORMAT, &StaticWindowValues),
        "[alpha] 5:logs, current pane 0 - (%H:%M %d-%b-%y)"
    );
}

#[test]
fn default_list_sessions_format_matches_canonical_output() {
    let rendered = render_template(DEFAULT_LIST_SESSIONS_FORMAT, &StaticWindowValues);
    let created = render_template("#{t:session_created}", &StaticWindowValues);

    assert_eq!(
        rendered,
        format!("alpha: 2 windows (created {created}) (attached)")
    );
}

#[test]
fn default_list_panes_format_matches_canonical_output() {
    let rendered = render_template(DEFAULT_LIST_PANES_FORMAT, &StaticWindowValues);

    assert_eq!(
        rendered,
        "alpha:5.0: [80x24] [history 10/2000, 512 bytes] %4 (active)"
    );
}

#[test]
fn hash_escaping_conditionals_and_unknowns_match_compatibility_behavior() {
    let rendered = render_template(
        "##{literal}#{?window_active,yes,no}/#{?window_last_flag,last,not-last}/#{missing}",
        &StaticWindowValues,
    );

    assert_eq!(rendered, "#{literal}yes/not-last/");
}

#[test]
fn modifiers_and_bare_aliases() {
    // Single-character tmux aliases expand through `format_value_by_name`,
    // and named runtime values stay reachable without growing the enum.
    let rendered = render_template(
        "#I/#W/#S/#T/#F/#{=21:pane_title}/#{E:session_name}/#{T:session_name}/#{window_flags}",
        &StaticWindowValues,
    );

    assert_eq!(
        rendered,
        "5/logs/alpha/build logs/*/build logs/alpha/alpha/*"
    );
}

#[test]
fn tmux_runtime_modifiers_cover_ascii_colour_width_and_name_existence() {
    let rendered = render_template(
            "#{a:98}|#{a:N}|#{c:red}|#{c:window_name}|#{w:window_name}|#{w:#{window_name}}|#{w:literal}|#{N:window_name}|#{N/w:logs}|#{N/s:alpha}|#{N/s:beta}",
            &StaticWindowValues,
        );

    assert_eq!(rendered, "b||800000||4|4|0|0|1|1|0");
}

#[test]
fn shared_truthiness_matches_the_compatibility_predicate() {
    assert!(!is_truthy(""));
    assert!(!is_truthy("0"));
    assert!(is_truthy("00"));
    assert!(is_truthy("false"));
}

#[test]
fn malformed_or_empty_expressions_keep_compatibility_behavior() {
    // Unclosed `#{` — no matching `}` found, tmux breaks out of loop.
    assert_eq!(render_template("#{window_name", &StaticWindowValues), "");

    // Conditional with empty branches.
    assert_eq!(
        render_template(
            "#{?window_active,,fallback}:#{?window_last_flag,last,}:#{?missing,yes,}",
            &StaticWindowValues,
        ),
        "::"
    );

    // Broken nesting — format_skip fails for the outer `#{}`, tmux drops
    // everything from the `#` onward.
    assert_eq!(
        render_template(
            "#{window_name/#{?window_active,yes}/tail",
            &StaticWindowValues
        ),
        ""
    );

    assert_eq!(
        render_template("a#{?b}z|#{#{#{x}}}", &StaticWindowValues),
        "a"
    );
    assert_eq!(render_template("#{#{#{x}}}", &StaticWindowValues), "");
}

#[test]
fn edge_case_empty_template_and_boundary_hash_patterns() {
    assert_eq!(render_template("", &StaticWindowValues), "");
    assert_eq!(render_template("#", &StaticWindowValues), "#");
    assert_eq!(render_template("##", &StaticWindowValues), "#");
    assert_eq!(render_template("###", &StaticWindowValues), "##");
    assert_eq!(render_template("####", &StaticWindowValues), "##");
    assert_eq!(render_template("text#", &StaticWindowValues), "text#");
    assert_eq!(render_template("#{}", &StaticWindowValues), "");
    // Unclosed `#{` — tmux drops everything from `#` onward.
    assert_eq!(render_template("#{", &StaticWindowValues), "");
}

#[test]
fn edge_case_conditional_with_missing_branches() {
    assert_eq!(
        render_template("#{?window_active}", &StaticWindowValues),
        ""
    );
    assert_eq!(
        render_template("a#{?window_active}z", &StaticWindowValues),
        "a"
    );
    assert_eq!(
        render_template("#{?#{window_active}}", &StaticWindowValues),
        ""
    );
    assert_eq!(
        render_template("#{?window_active,yes}", &StaticWindowValues),
        ""
    );
    assert_eq!(
        render_template("#{?#{window_active},yes}", &StaticWindowValues),
        ""
    );
    assert_eq!(
        render_template("#{?#{window_last_flag},yes}", &StaticWindowValues),
        ""
    );
}

#[test]
fn nested_conditionals_render_inner_templates_in_selected_branches() {
    assert_eq!(
        render_template("#{?window_active,#{window_name},no}", &StaticWindowValues),
        "logs"
    );
    assert_eq!(
        render_template(
            "#{?window_last_flag,#{window_name},#{session_name}}",
            &StaticWindowValues
        ),
        "alpha"
    );
    assert_eq!(
        render_template(
            "#{?window_last_flag,yes,#{?window_active,#{window_name},no}}",
            &StaticWindowValues
        ),
        "logs"
    );
}

#[test]
fn multi_pair_conditional_default_can_contain_nested_conditionals() {
    let session = Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 });
    let context = FormatContext::from_session(&session).with_session_attached(1);

    assert_eq!(
        render_template(
            "#{?pane_format,pane,window_format,window,#{session_windows} windows#{?session_attached, (attached),}}",
            &context,
        ),
        "1 windows (attached)"
    );
}

#[test]
fn edge_case_unicode_in_template_text_preserved() {
    assert_eq!(
        render_template("🪟 #{window_name} — #{session_name}", &StaticWindowValues),
        "🪟 logs — alpha"
    );
}

#[test]
fn edge_case_session_only_context_leaves_window_and_pane_variables_empty() {
    let session = Session::new(session_name("beta"), TerminalSize { cols: 80, rows: 24 });
    let context = FormatContext::from_session(&session);

    let rendered = render_template(
        "#{session_name}|#{window_index}|#{window_name}|#{window_raw_flags}|#{pane_index}",
        &context,
    );

    assert_eq!(rendered, "beta||||");
}

#[test]
fn format_context_populates_new_session_and_pane_values() {
    let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 });
    session.split_active_pane().expect("split succeeds");
    let pane = session.window().active_pane().expect("active pane exists");
    let geometry = pane.geometry();
    let context = FormatContext::from_session(&session)
        .with_session_attached(3)
        .with_window(session.active_window_index(), session.window(), true, false)
        .with_window_pane(session.window(), pane);

    let rendered = render_template(
            "#{session_name}:#{session_windows}:#{session_attached}:#{session_width}x#{session_height}:#{pane_index}:#{pane_id}:#{pane_active}:#{pane_width}x#{pane_height}",
            &context,
        );

    assert_eq!(
        rendered,
        format!(
            "alpha:1:3:80x24:{}:%{}:1:{}x{}",
            pane.index(),
            pane.id().as_u32(),
            geometry.cols(),
            geometry.rows()
        )
    );
}

// -----------------------------------------------------------------------
// New tests — format_skip
// -----------------------------------------------------------------------
