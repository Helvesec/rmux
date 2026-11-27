use std::time::Duration;

use super::{
    lag_json_value, observe_needle, send_keys_wait_exit_code, PaneExitStatus, WaitCondition,
};

#[test]
fn next_text_observer_matches_across_output_chunks() {
    let mut tail = Vec::new();

    assert!(!observe_needle(&mut tail, b"abc", b"cde"));
    assert!(observe_needle(&mut tail, b"def", b"cde"));
}

#[test]
fn next_text_observer_keeps_only_required_tail() {
    let mut tail = Vec::new();

    assert!(!observe_needle(&mut tail, b"abcdef", b"defgh"));

    assert_eq!(tail, b"cdef");
}

#[test]
fn next_text_lag_json_is_machine_readable() {
    let value = lag_json_value("next-text", "%1", 7);

    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"], "pane-output-lag");
    assert_eq!(value["condition"], "next-text");
    assert_eq!(value["target"], "%1");
    assert_eq!(value["missed_events"], 7);
}

#[test]
fn quiet_completion_does_not_claim_the_observed_process_status() {
    let condition = WaitCondition::Quiet(Duration::from_millis(100));

    assert_eq!(
        send_keys_wait_exit_code(&condition, Some(PaneExitStatus::known(Some(7), None)),)
            .expect("quiet reports completion rather than process success"),
        0
    );
}
