use std::path::Path;

use rmux_core::OptionStore;
use rmux_proto::OptionName;

use crate::client_format::{ClientFormatBindings, ClientLoopSnapshot};
use crate::format_runtime::RuntimeFormatContext;
use crate::pane_terminals::HandlerState;

use super::status::{active_format_context, render_status_template_jobs, status_job_cache_ttl};

/// The client-scoped bindings tmux installs with `format_defaults()` before it
/// expands `set-titles-string`.
///
/// `client` carries the active client's own `#{client_*}` values. tmux
/// initialises the title's format tree from the client it is about to write to,
/// so without them every `#{client_width}`, `#{client_height}` and
/// `#{client_name}` in a custom title expands empty (issue #182). It is `None`
/// only where no single client owns the render — the web and snapshot paths,
/// which emit no title at all.
pub(crate) struct ClientTitleContext<'a> {
    pub(crate) state: Option<&'a HandlerState>,
    pub(crate) attached_count: usize,
    pub(crate) key_table: Option<&'a str>,
    pub(crate) socket_path: Option<&'a Path>,
    pub(crate) client: Option<&'a ClientFormatBindings>,
    pub(crate) client_loop_snapshots: Option<&'a [ClientLoopSnapshot]>,
}

/// Expands `set-titles-string` for one attached client.
///
/// `None` means "this client must not drive the outer terminal at all": tmux
/// only reaches `server_client_set_title()` / `server_client_set_path()` when
/// the session's `set-titles` is on, so with it off neither the terminal title
/// nor the OSC 7 path is written. Measured on tmux 3.7b: with `set-titles off`
/// an attached client emits no OSC 0 and no OSC 7 even when the terminal
/// advertises both capabilities, and a `#()` job in the template is never run.
pub(crate) fn expand_client_title(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
    context: &ClientTitleContext<'_>,
) -> Option<String> {
    let session_name = session.name();
    if options.resolve(Some(session_name), OptionName::SetTitles) != Some("on") {
        return None;
    }

    let template = options
        .resolve(Some(session_name), OptionName::SetTitlesString)
        .unwrap_or_default();

    let mut runtime = RuntimeFormatContext::new(active_format_context(
        session,
        context.attached_count,
        context.key_table,
        context.socket_path,
    ))
    .with_options(options)
    .with_session(session)
    .with_window(session.active_window_index(), session.window());
    if let Some(state) = context.state {
        runtime = runtime.with_state(state);
    }
    if let Some(pane) = session.window().active_pane() {
        runtime = runtime.with_pane(pane);
    }
    if let Some(client) = context.client {
        runtime = client.apply(runtime);
    }
    if let Some(client_loop_snapshots) = context.client_loop_snapshots {
        runtime = runtime.with_client_loop_snapshots(client_loop_snapshots);
    }

    // tmux expands the title through format_expand_time(), so strftime tokens
    // resolve against the current time and `#(shell-command)` runs as a job.
    // Measured on tmux 3.7b: the first expansion of a job is empty and each
    // later one carries the previous run's stdout, re-run no more often than
    // `status-interval`. That is exactly the daemon's status-job runtime, so
    // the title shares its cache, its concurrency limit and its cancellation.
    Some(render_status_template_jobs(
        template,
        &runtime,
        status_job_cache_ttl(options, session_name),
    ))
}
