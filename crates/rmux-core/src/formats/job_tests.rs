use std::cell::RefCell;

use super::{render_template, render_template_preserving_jobs, FormatVariable, FormatVariables};

#[derive(Default)]
struct Jobs {
    calls: RefCell<Vec<(String, String)>>,
}

impl FormatVariables for Jobs {
    fn format_value(&self, _: FormatVariable) -> Option<String> {
        None
    }

    fn format_value_by_name(&self, name: &str) -> Option<String> {
        match name {
            "@job" => Some("#(printf expanded)".to_owned()),
            "@recursive" => Some("#{E:@recursive}".to_owned()),
            "session_name" => Some("alpha".to_owned()),
            _ => None,
        }
    }

    fn format_job(&self, source: &str, command: &str) -> Option<String> {
        self.calls
            .borrow_mut()
            .push((source.to_owned(), command.to_owned()));
        Some("#{session_name}#(literal-output)".to_owned())
    }
}

#[test]
fn runtime_job_callback_runs_only_in_selected_unescaped_expressions() {
    let jobs = Jobs::default();
    assert_eq!(
        render_template("##(escaped)|#{?0,#(unselected),yes}", &jobs),
        "#(escaped)|yes"
    );
    assert!(jobs.calls.borrow().is_empty());
    assert_eq!(
        render_template("#{E:@job}", &jobs),
        "#{session_name}#(literal-output)"
    );
    assert_eq!(
        *jobs.calls.borrow(),
        [("printf expanded".into(), "printf expanded".into())]
    );
}

#[test]
fn job_command_uses_current_variables_without_nested_job_execution() {
    let jobs = Jobs::default();
    assert_eq!(
        render_template("#(echo #{session_name} #(nested))", &jobs),
        "#{session_name}#(literal-output)"
    );
    assert_eq!(
        *jobs.calls.borrow(),
        [(
            "echo #{session_name} #(nested)".into(),
            "echo alpha ".into()
        )]
    );
}

#[test]
fn preserving_jobs_and_malformed_tail_have_no_execution_authority() {
    let jobs = Jobs::default();
    assert_eq!(
        render_template_preserving_jobs("#{E:@job}", &jobs),
        "#(printf expanded)"
    );
    assert_eq!(render_template("before#(unfinished", &jobs), "before");
    assert!(jobs.calls.borrow().is_empty());
}

#[test]
fn job_command_reexpansion_keeps_the_shared_recursion_limit() {
    let jobs = Jobs::default();
    let _ = render_template("#(#{E:@recursive})", &jobs);
    assert!(jobs.calls.borrow().len() <= 1);
    assert!(jobs
        .calls
        .borrow()
        .iter()
        .all(|(_, command)| command.is_empty()));
}
