use super::super::super::scripting_support::QueueExecutionContext;
use super::super::super::{DetachedRequesterAuthority, RequesterOrigin};
use super::super::{
    CommandPromptPlan, ConfirmBeforePlan, PromptCompletion, PromptField, PromptType,
};
use super::*;

fn test_origin(requester_pid: u32) -> RequesterOrigin {
    RequesterOrigin::new(requester_pid, DetachedRequesterAuthority::Denied)
}

#[test]
fn command_prompt_initial_render_starts_in_entry_mode() {
    let plan = CommandPromptPlan {
        origin: test_origin(1),
        target_client: None,
        context: QueueExecutionContext::without_caller_cwd(),
        fields: vec![PromptField {
            prompt: "(rename-window) ".to_owned(),
            input: "bash".to_owned(),
        }],
        template: "rename-window -- '%%'".to_owned(),
        flags: 0,
        prompt_type: PromptType::Command,
        background: false,
        format_values: Vec::new(),
    };

    let prompt =
        ClientPromptState::new_command(plan, PromptCompletion::Background).rendered_prompt();
    assert!(!prompt.command_prompt);
    assert_eq!(prompt.prompt, "(rename-window) ");
    assert_eq!(prompt.input, "bash");
}

#[test]
fn buffer_operations_with_multibyte_chars() {
    let plan = CommandPromptPlan {
        origin: test_origin(1),
        target_client: None,
        context: QueueExecutionContext::without_caller_cwd(),
        fields: vec![PromptField {
            prompt: ":".to_owned(),
            input: String::new(),
        }],
        template: "%%".to_owned(),
        flags: 0,
        prompt_type: PromptType::Command,
        background: false,
        format_values: Vec::new(),
    };
    let mut prompt = ClientPromptState::new_command(plan, PromptCompletion::Background);

    prompt.push_char('ñ');
    prompt.push_char('日');
    prompt.push_char('本');
    assert_eq!(prompt.buffer, "ñ日本");
    assert_eq!(prompt.cursor, 3);

    prompt.move_left();
    assert_eq!(prompt.cursor, 2);

    prompt.delete_at_cursor();
    assert_eq!(prompt.buffer, "ñ日");

    prompt.delete_left();
    assert_eq!(prompt.buffer, "ñ");
    assert_eq!(prompt.cursor, 1);

    prompt.move_home();
    assert_eq!(prompt.cursor, 0);

    prompt.delete_to_end();
    assert!(prompt.buffer.is_empty());
}

#[test]
fn batched_text_inserts_once_at_the_unicode_cursor() {
    let plan = CommandPromptPlan {
        origin: test_origin(1),
        target_client: None,
        context: QueueExecutionContext::without_caller_cwd(),
        fields: vec![PromptField {
            prompt: ":".to_owned(),
            input: "a日本z".to_owned(),
        }],
        template: "%%".to_owned(),
        flags: 0,
        prompt_type: PromptType::Command,
        background: false,
        format_values: Vec::new(),
    };
    let mut prompt = ClientPromptState::new_command(plan, PromptCompletion::Background);
    assert!(prompt.move_left());

    assert!(prompt.insert_batched_text("é🙂"));
    assert_eq!(prompt.buffer, "a日本é🙂z");
    assert_eq!(prompt.cursor, 5);
}

#[test]
fn per_event_prompt_types_reject_batched_text() {
    for flags in [
        PROMPT_FLAG_SINGLE,
        PROMPT_FLAG_NUMERIC,
        PROMPT_FLAG_KEY,
        PROMPT_FLAG_INCREMENTAL,
    ] {
        let plan = CommandPromptPlan {
            origin: test_origin(1),
            target_client: None,
            context: QueueExecutionContext::without_caller_cwd(),
            fields: vec![PromptField {
                prompt: ":".to_owned(),
                input: String::new(),
            }],
            template: "%%".to_owned(),
            flags,
            prompt_type: PromptType::Command,
            background: flags == PROMPT_FLAG_INCREMENTAL,
            format_values: Vec::new(),
        };
        let mut prompt = ClientPromptState::new_command(plan, PromptCompletion::Background);
        assert!(!prompt.insert_batched_text("é"), "flags={flags:#x}");
        assert!(prompt.buffer.is_empty());
        assert_eq!(prompt.cursor, 0);
    }
}

#[test]
fn confirm_key_mode_accepts_correct_key() {
    let plan = ConfirmBeforePlan {
        origin: test_origin(1),
        target_client: None,
        context: QueueExecutionContext::without_caller_cwd(),
        prompt: "sure? ".to_owned(),
        template: "kill-window".to_owned(),
        confirm_key: 'y',
        default_yes: false,
        background: false,
        format_values: Vec::new(),
    };
    let mut prompt = ClientPromptState::new_confirm(plan, PromptCompletion::Background);
    let mut history = PromptHistoryStore::default();

    let action = process_prompt_event(
        &mut prompt,
        PromptInputEvent::Char('n'),
        &mut history,
        "",
        100,
    );
    assert!(action.finalize.is_some());
    match action.finalize.unwrap() {
        PromptFinalizeKind::Confirm { accepted } => assert!(!accepted),
        other => panic!("expected Confirm, got {other:?}"),
    }
}

#[test]
fn confirm_enter_without_default_yes_declines() {
    let plan = ConfirmBeforePlan {
        origin: test_origin(1),
        target_client: None,
        context: QueueExecutionContext::without_caller_cwd(),
        prompt: "sure? ".to_owned(),
        template: "kill-window".to_owned(),
        confirm_key: 'y',
        default_yes: false,
        background: false,
        format_values: Vec::new(),
    };
    let mut prompt = ClientPromptState::new_confirm(plan, PromptCompletion::Background);
    let mut history = PromptHistoryStore::default();

    let action = process_prompt_event(&mut prompt, PromptInputEvent::Enter, &mut history, "", 100);
    assert!(action.finalize.is_some());
    match action.finalize.unwrap() {
        PromptFinalizeKind::Confirm { accepted } => assert!(!accepted),
        other => panic!("expected Confirm, got {other:?}"),
    }
}

#[test]
fn confirm_enter_with_default_yes_accepts() {
    let plan = ConfirmBeforePlan {
        origin: test_origin(1),
        target_client: None,
        context: QueueExecutionContext::without_caller_cwd(),
        prompt: "sure? ".to_owned(),
        template: "kill-window".to_owned(),
        confirm_key: 'y',
        default_yes: true,
        background: false,
        format_values: Vec::new(),
    };
    let mut prompt = ClientPromptState::new_confirm(plan, PromptCompletion::Background);
    let mut history = PromptHistoryStore::default();

    let action = process_prompt_event(&mut prompt, PromptInputEvent::Enter, &mut history, "", 100);
    assert!(action.finalize.is_some());
    match action.finalize.unwrap() {
        PromptFinalizeKind::Confirm { accepted } => assert!(accepted),
        other => panic!("expected Confirm, got {other:?}"),
    }
}

#[test]
fn key_mode_captures_any_key() {
    let plan = CommandPromptPlan {
        origin: test_origin(1),
        target_client: None,
        context: QueueExecutionContext::without_caller_cwd(),
        fields: vec![PromptField {
            prompt: "key: ".to_owned(),
            input: String::new(),
        }],
        template: "%%".to_owned(),
        flags: PROMPT_FLAG_KEY,
        prompt_type: PromptType::Command,
        background: false,
        format_values: Vec::new(),
    };
    let mut prompt = ClientPromptState::new_command(plan, PromptCompletion::Background);
    let mut history = PromptHistoryStore::default();

    let action = process_prompt_event(&mut prompt, PromptInputEvent::Up, &mut history, "", 100);
    assert!(action.finalize.is_some());
}

#[test]
fn numeric_mode_rejects_non_digits() {
    let plan = CommandPromptPlan {
        origin: test_origin(1),
        target_client: None,
        context: QueueExecutionContext::without_caller_cwd(),
        fields: vec![PromptField {
            prompt: "num: ".to_owned(),
            input: String::new(),
        }],
        template: "%%".to_owned(),
        flags: PROMPT_FLAG_NUMERIC,
        prompt_type: PromptType::Command,
        background: false,
        format_values: Vec::new(),
    };
    let mut prompt = ClientPromptState::new_command(plan, PromptCompletion::Background);
    let mut history = PromptHistoryStore::default();

    let action = process_prompt_event(
        &mut prompt,
        PromptInputEvent::Char('5'),
        &mut history,
        "",
        100,
    );
    assert!(action.finalize.is_none());
    assert_eq!(prompt.buffer, "5");

    let action = process_prompt_event(
        &mut prompt,
        PromptInputEvent::Char('a'),
        &mut history,
        "",
        100,
    );
    assert!(action.finalize.is_some());
}

#[test]
fn incremental_mode_dispatches_on_each_char() {
    let plan = CommandPromptPlan {
        origin: test_origin(1),
        target_client: None,
        context: QueueExecutionContext::without_caller_cwd(),
        fields: vec![PromptField {
            prompt: "(search) ".to_owned(),
            input: String::new(),
        }],
        template: "%%".to_owned(),
        flags: PROMPT_FLAG_INCREMENTAL,
        prompt_type: PromptType::Search,
        background: true,
        format_values: Vec::new(),
    };
    let mut prompt = ClientPromptState::new_command(plan, PromptCompletion::Background);
    let mut history = PromptHistoryStore::default();

    assert!(prompt.initial_incremental_dispatch().is_some());

    let action = process_prompt_event(
        &mut prompt,
        PromptInputEvent::Char('a'),
        &mut history,
        "",
        100,
    );
    assert!(action.dispatch.is_some());
    let dispatch = action.dispatch.unwrap();
    assert_eq!(dispatch.responses, vec!["=a"]);

    let action = process_prompt_event(&mut prompt, PromptInputEvent::Enter, &mut history, "", 100);
    assert!(matches!(action.finalize, Some(PromptFinalizeKind::Cancel)));
}

#[test]
fn bspace_exit_cancels_on_empty_buffer_backspace() {
    let plan = CommandPromptPlan {
        origin: test_origin(1),
        target_client: None,
        context: QueueExecutionContext::without_caller_cwd(),
        fields: vec![PromptField {
            prompt: "> ".to_owned(),
            input: String::new(),
        }],
        template: "%%".to_owned(),
        flags: PROMPT_FLAG_BSPACE_EXIT,
        prompt_type: PromptType::Command,
        background: false,
        format_values: Vec::new(),
    };
    let mut prompt = ClientPromptState::new_command(plan, PromptCompletion::Background);
    let mut history = PromptHistoryStore::default();

    let action = process_prompt_event(
        &mut prompt,
        PromptInputEvent::Backspace,
        &mut history,
        "",
        100,
    );
    assert!(matches!(action.finalize, Some(PromptFinalizeKind::Cancel)));
}

#[test]
fn delete_word_left_and_paste() {
    let plan = CommandPromptPlan {
        origin: test_origin(1),
        target_client: None,
        context: QueueExecutionContext::without_caller_cwd(),
        fields: vec![PromptField {
            prompt: ":".to_owned(),
            input: "hello world".to_owned(),
        }],
        template: "%%".to_owned(),
        flags: 0,
        prompt_type: PromptType::Command,
        background: false,
        format_values: Vec::new(),
    };
    let mut prompt = ClientPromptState::new_command(plan, PromptCompletion::Background);
    assert_eq!(prompt.cursor, 11);

    prompt.delete_word_left(" ");
    assert_eq!(prompt.buffer, "hello ");
    assert_eq!(prompt.saved, "world");
    assert_eq!(prompt.cursor, 6);

    prompt.paste_saved();
    assert_eq!(prompt.buffer, "hello world");
    assert_eq!(prompt.cursor, 11);
}

#[test]
fn confirm_key_y_accepts_y_char() {
    let plan = ConfirmBeforePlan {
        origin: test_origin(1),
        target_client: None,
        context: QueueExecutionContext::without_caller_cwd(),
        prompt: "kill? ".to_owned(),
        template: "kill-window".to_owned(),
        confirm_key: 'y',
        default_yes: false,
        background: false,
        format_values: Vec::new(),
    };
    let mut prompt = ClientPromptState::new_confirm(plan, PromptCompletion::Background);
    let mut history = PromptHistoryStore::default();

    let action = process_prompt_event(
        &mut prompt,
        PromptInputEvent::Char('y'),
        &mut history,
        "",
        100,
    );
    match action.finalize.unwrap() {
        PromptFinalizeKind::Confirm { accepted } => assert!(accepted),
        other => panic!("expected accepted Confirm, got {other:?}"),
    }
}

#[test]
fn confirm_escape_cancels() {
    let plan = ConfirmBeforePlan {
        origin: test_origin(1),
        target_client: None,
        context: QueueExecutionContext::without_caller_cwd(),
        prompt: "kill? ".to_owned(),
        template: "kill-window".to_owned(),
        confirm_key: 'y',
        default_yes: false,
        background: false,
        format_values: Vec::new(),
    };
    let mut prompt = ClientPromptState::new_confirm(plan, PromptCompletion::Background);
    let mut history = PromptHistoryStore::default();

    let action = process_prompt_event(&mut prompt, PromptInputEvent::Escape, &mut history, "", 100);
    match action.finalize.unwrap() {
        PromptFinalizeKind::Cancel => {}
        other => panic!("expected Cancel, got {other:?}"),
    }
}

#[test]
fn confirm_ctrl_c_cancels() {
    let plan = ConfirmBeforePlan {
        origin: test_origin(1),
        target_client: None,
        context: QueueExecutionContext::without_caller_cwd(),
        prompt: "kill? ".to_owned(),
        template: "kill-window".to_owned(),
        confirm_key: 'y',
        default_yes: false,
        background: false,
        format_values: Vec::new(),
    };
    let mut prompt = ClientPromptState::new_confirm(plan, PromptCompletion::Background);
    let mut history = PromptHistoryStore::default();

    let action = process_prompt_event(
        &mut prompt,
        PromptInputEvent::Ctrl('c'),
        &mut history,
        "",
        100,
    );
    match action.finalize.unwrap() {
        PromptFinalizeKind::Cancel => {}
        other => panic!("expected Cancel, got {other:?}"),
    }
}

#[test]
fn numeric_mode_escape_cancels() {
    let plan = CommandPromptPlan {
        origin: test_origin(1),
        target_client: None,
        context: QueueExecutionContext::without_caller_cwd(),
        fields: vec![PromptField {
            prompt: "num: ".to_owned(),
            input: String::new(),
        }],
        template: "%%".to_owned(),
        flags: PROMPT_FLAG_NUMERIC,
        prompt_type: PromptType::Command,
        background: false,
        format_values: Vec::new(),
    };
    let mut prompt = ClientPromptState::new_command(plan, PromptCompletion::Background);
    let mut history = PromptHistoryStore::default();

    prompt.push_char('3');
    let action = process_prompt_event(&mut prompt, PromptInputEvent::Escape, &mut history, "", 100);
    assert!(matches!(action.finalize, Some(PromptFinalizeKind::Cancel)));
}

#[test]
fn numeric_mode_backspace_on_empty_submits_empty() {
    let plan = CommandPromptPlan {
        origin: test_origin(1),
        target_client: None,
        context: QueueExecutionContext::without_caller_cwd(),
        fields: vec![PromptField {
            prompt: "num: ".to_owned(),
            input: String::new(),
        }],
        template: "%%".to_owned(),
        flags: PROMPT_FLAG_NUMERIC,
        prompt_type: PromptType::Command,
        background: false,
        format_values: Vec::new(),
    };
    let mut prompt = ClientPromptState::new_command(plan, PromptCompletion::Background);
    let mut history = PromptHistoryStore::default();

    let action = process_prompt_event(
        &mut prompt,
        PromptInputEvent::Backspace,
        &mut history,
        "",
        100,
    );
    assert!(action.finalize.is_some());
}

#[test]
fn multi_prompt_advances_through_fields() {
    let plan = CommandPromptPlan {
        origin: test_origin(1),
        target_client: None,
        context: QueueExecutionContext::without_caller_cwd(),
        fields: vec![
            PromptField {
                prompt: "first: ".to_owned(),
                input: String::new(),
            },
            PromptField {
                prompt: "second: ".to_owned(),
                input: "default2".to_owned(),
            },
        ],
        template: "%% %2".to_owned(),
        flags: 0,
        prompt_type: PromptType::Command,
        background: false,
        format_values: Vec::new(),
    };
    let mut prompt = ClientPromptState::new_command(plan, PromptCompletion::Background);
    assert_eq!(prompt.prompt, "first: ");
    assert_eq!(prompt.buffer, "");

    let result = prompt.submit_response("alpha".to_owned());
    assert!(result.is_none());
    assert_eq!(prompt.prompt, "second: ");
    assert_eq!(prompt.buffer, "default2");
    assert_eq!(prompt.cursor, 8);

    let result = prompt.submit_response("beta".to_owned());
    assert!(result.is_some());
}

#[test]
fn incremental_ctrl_r_with_empty_buffer_restores_last_input() {
    let plan = CommandPromptPlan {
        origin: test_origin(1),
        target_client: None,
        context: QueueExecutionContext::without_caller_cwd(),
        fields: vec![PromptField {
            prompt: "(search) ".to_owned(),
            input: "previous".to_owned(),
        }],
        template: "%%".to_owned(),
        flags: PROMPT_FLAG_INCREMENTAL,
        prompt_type: PromptType::Search,
        background: true,
        format_values: Vec::new(),
    };
    let mut prompt = ClientPromptState::new_command(plan, PromptCompletion::Background);
    let mut history = PromptHistoryStore::default();

    assert!(prompt.buffer.is_empty());
    assert_eq!(prompt.last_input, "previous");

    let action = process_prompt_event(
        &mut prompt,
        PromptInputEvent::Ctrl('r'),
        &mut history,
        "",
        100,
    );
    assert_eq!(prompt.buffer, "previous");
    assert!(action.dispatch.is_some());
    let dispatch = action.dispatch.unwrap();
    assert_eq!(dispatch.responses, vec!["=previous"]);
}
