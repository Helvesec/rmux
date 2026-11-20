//! One top-level expansion is bounded by time, not only by depth (R3332 H1).
//!
//! The expectations are the stdout of tmux 3.7b (`/usr/local/bin/tmux` on a
//! Linux host), measured on a private socket with `-f /dev/null` before this
//! correction: `display -p '#{E:@a}'` with `@a = '#{E:@a}#{E:@a}'` printed an
//! empty line in 0.107 s, `A#{E:@a}B#{E:@a}C` printed `AB`, `y#{E:@c}` with
//! `@c = 'y#{E:@c}'` printed 99 `y`, and `#{?#{E:@a},yes,no}` printed an empty
//! line: the condition spends the budget, so no branch is expanded either.
//!
//! The time limit applies once an expansion has scanned more than
//! `FORMAT_TIME_FLOOR_BYTES` of format text, and a ceiling of ten times that
//! limit applies whatever the volume once it has entered more than
//! `FORMAT_CEILING_FLOOR_ENTRIES` nested expansions; see `budget.rs` for why.

use std::rc::Rc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use super::super::budget::{
    ExpandBudget, WorkTimeLimit, FORMAT_CEILING_FLOOR_ENTRIES, FORMAT_TIME_CEILING,
    FORMAT_TIME_FLOOR_BYTES, FORMAT_TIME_LIMIT,
};
use super::super::{format_expand1, ExpandState};
use super::*;

/// User options whose values re-expand themselves.
struct SelfExpandingOptions;

impl FormatVariables for SelfExpandingOptions {
    fn format_value(&self, _variable: FormatVariable) -> Option<String> {
        None
    }

    fn format_value_by_name(&self, name: &str) -> Option<String> {
        match name {
            // Two references per level: 2^100 expansions under the depth limit.
            "@a" => Some("#{E:@a}#{E:@a}".to_owned()),
            // One reference per level: only the depth limit applies.
            "@c" => Some("y#{E:@c}".to_owned()),
            _ => None,
        }
    }
}

/// Renders on a worker so that an unbounded expansion fails the test instead
/// of hanging it.
fn render_within(template: &'static str, deadline: Duration) -> Option<String> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(render_template(template, &SelfExpandingOptions));
    });
    receiver.recv_timeout(deadline).ok()
}

#[test]
fn fan_out_expansion_stops_at_the_time_budget() {
    let rendered = render_within("#{E:@a}", Duration::from_secs(10));
    assert_eq!(rendered.as_deref(), Some(""));
}

#[test]
fn spent_budget_stops_scanning_at_the_next_expression() {
    for (template, expected) in [
        ("A#{E:@a}B", "AB"),
        ("A#{E:@a}B#{E:@a}C", "AB"),
        ("#{l:x}#{E:@a}#{l:y}", "x"),
    ] {
        let rendered = render_within(template, Duration::from_secs(10));
        assert_eq!(rendered.as_deref(), Some(expected), "{template}");
    }
}

#[test]
fn spent_budget_reaches_the_branches_of_a_condition() {
    let rendered = render_within("#{?#{E:@a},yes,no}", Duration::from_secs(10));
    assert_eq!(rendered.as_deref(), Some(""));
}

#[test]
fn linear_self_reference_still_ends_at_the_depth_limit() {
    let rendered = render_template("#{E:@c}", &SelfExpandingOptions);
    assert_eq!(rendered, "y".repeat(99));
}

fn render_with_budget<V>(template: &str, variables: &V, budget: ExpandBudget) -> String
where
    V: FormatVariables + ?Sized,
{
    let mut state = ExpandState {
        loop_depth: 0,
        expand_time: false,
        stop_expansion: false,
        preserve_jobs: false,
        disable_jobs: false,
        budget: Rc::new(budget),
    };
    format_expand1(&mut state, template, variables)
}

const TMUX_LIMIT: WorkTimeLimit = WorkTimeLimit {
    floor: FORMAT_TIME_FLOOR_BYTES,
    limit: FORMAT_TIME_LIMIT,
};

const CEILING: WorkTimeLimit = WorkTimeLimit {
    floor: FORMAT_CEILING_FLOOR_ENTRIES,
    limit: FORMAT_TIME_CEILING,
};

fn spent(floor: usize) -> WorkTimeLimit {
    WorkTimeLimit {
        floor,
        limit: Duration::ZERO,
    }
}

#[test]
fn spent_time_past_the_work_floor_expands_nothing() {
    for budget in [
        ExpandBudget::with_limits(spent(0), CEILING),
        ExpandBudget::with_limits(TMUX_LIMIT, spent(0)),
    ] {
        budget.charge(1);
        assert_eq!(
            render_with_budget("plain text", &StaticWindowValues, budget),
            ""
        );
    }
}

/// RMUX divergence from tmux 3.7b: below the work floors the clock is not
/// read, so an ordinary expansion that a descheduled thread finishes late is
/// whole, even with both limits already spent.
#[test]
fn ordinary_expansion_below_the_work_floor_ignores_spent_time() {
    let late = || {
        ExpandBudget::with_limits(
            spent(FORMAT_TIME_FLOOR_BYTES),
            spent(FORMAT_CEILING_FLOOR_ENTRIES),
        )
    };
    assert_eq!(
        render_with_budget(
            "#{session_name}:#{window_index}",
            &StaticWindowValues,
            late()
        ),
        "alpha:5"
    );
    // One hundred nested expansions, the depth limit, stay below both floors.
    assert_eq!(
        render_with_budget("#{E:@c}", &SelfExpandingOptions, late()),
        "y".repeat(99)
    );
    assert_eq!(
        render_with_budget("#{E:@a}", &SelfExpandingOptions, late()),
        ""
    );
}

/// User options whose leaves are cheap in format bytes but costly in time,
/// like `#{C:zzz}` over a long history (R3343: 5.1 s for the byte floor alone
/// over 50 000 lines, while tmux 3.7b answers in 0.106 s).
struct CostlyLeafOptions;

impl FormatVariables for CostlyLeafOptions {
    fn format_value(&self, _variable: FormatVariable) -> Option<String> {
        None
    }

    fn format_value_by_name(&self, name: &str) -> Option<String> {
        (name == "@a").then(|| "#{C:zzz}#{E:@a}#{E:@a}".to_owned())
    }

    fn format_search(&self, _options: &str, _pattern: &str) -> Option<String> {
        thread::sleep(Duration::from_millis(1));
        None
    }
}

#[test]
fn costly_leaves_stop_at_the_ceiling_whatever_the_volume() {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let started = Instant::now();
        let rendered = render_template("#{E:@a}", &CostlyLeafOptions);
        let _ = sender.send((rendered, started.elapsed()));
    });
    // The byte floor alone lets this run for about ten seconds.
    let (rendered, elapsed) = receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("costly leaves rendered past the ceiling");
    assert_eq!(rendered, "");
    assert!(elapsed >= FORMAT_TIME_CEILING, "{elapsed:?}");
    assert!(elapsed < Duration::from_millis(1500), "{elapsed:?}");
}
