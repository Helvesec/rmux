//! Declarative pane layout builders.
//!
//! The layout builder is SDK-side composition. It creates panes through the
//! public split/spawn surfaces, asks the daemon to spread the resulting tree,
//! and returns stable [`PaneSet`] handles in declaration order.

use std::path::PathBuf;

use crate::handles::session::unexpected_response;
use crate::{
    Pane, PaneSet, ProcessCommandSpec, Result, RmuxError, Session, SplitDirection, WindowRef,
};
use rmux_proto::{Request, Response, SelectLayoutTarget, SpreadLayoutRequest};

/// Entry point returned by [`Session::layout`].
#[derive(Debug)]
pub struct SessionLayoutBuilder<'a> {
    session: &'a Session,
    window_index: u32,
}

impl<'a> SessionLayoutBuilder<'a> {
    pub(crate) const fn new(session: &'a Session) -> Self {
        Self {
            session,
            window_index: 0,
        }
    }

    /// Selects the window index that will receive the layout.
    ///
    /// This layout API mutates one existing window. The target window must
    /// already exist and must contain exactly one pane when [`GridLayoutBuilder::apply`]
    /// is called.
    #[must_use]
    pub const fn window(mut self, window_index: u32) -> Self {
        self.window_index = window_index;
        self
    }

    /// Starts a grid layout.
    ///
    /// The first argument is the maximum number of columns per row, matching
    /// the grid example `grid(3, 2)` for "three panes on top, two
    /// panes below". The second argument is the maximum number of rows.
    #[must_use]
    pub const fn grid(self, columns: usize, rows: usize) -> GridLayoutBuilder<'a> {
        GridLayoutBuilder {
            session: self.session,
            window_index: self.window_index,
            columns,
            rows,
            replace_existing_root_process: true,
            replace_existing_panes: false,
            panes: Vec::new(),
        }
    }
}

/// Builder for a single-window pane grid.
#[derive(Debug)]
pub struct GridLayoutBuilder<'a> {
    session: &'a Session,
    window_index: u32,
    columns: usize,
    rows: usize,
    replace_existing_root_process: bool,
    replace_existing_panes: bool,
    panes: Vec<LayoutPaneSpec>,
}

impl<'a> GridLayoutBuilder<'a> {
    /// Controls whether the first pane spec may replace the existing root
    /// pane process.
    ///
    /// The default is `true` because newly-created app sessions already have
    /// a placeholder shell in pane `0`; a command on the first declared pane
    /// is expected to replace that placeholder. Set this to `false` when
    /// applying a layout to an existing session should surface
    /// [`RmuxError::ProcessStillRunning`] instead of replacing the root
    /// process.
    #[must_use]
    pub const fn replace_existing_root_process(mut self, replace: bool) -> Self {
        self.replace_existing_root_process = replace;
        self
    }

    /// Allows applying the grid to a window that already contains multiple
    /// panes by closing every pane except the first listed pane before
    /// creating the requested layout.
    ///
    /// The default is `false`, which keeps the builder conservative on
    /// existing workspaces. When enabled, cleanup happens before any new split
    /// is created; if cleanup fails, the builder returns that error without
    /// attempting a partial layout.
    #[must_use]
    pub const fn replace_existing_panes(mut self, replace: bool) -> Self {
        self.replace_existing_panes = replace;
        self
    }

    /// Adds one pane declaration with a UX title label.
    ///
    /// Titles remain labels only; the returned handles are addressed by
    /// stable pane id after the layout is applied.
    #[must_use]
    pub fn pane(self, title: impl Into<String>) -> LayoutPaneBuilder<'a> {
        LayoutPaneBuilder {
            builder: self,
            spec: LayoutPaneSpec::new(title.into()),
        }
    }

    /// Applies the grid and returns stable pane handles in declaration order.
    pub async fn apply(self) -> Result<PaneSet> {
        apply_grid(self).await
    }
}

/// Builder for the pane most recently declared with
/// [`GridLayoutBuilder::pane`].
#[derive(Debug)]
pub struct LayoutPaneBuilder<'a> {
    builder: GridLayoutBuilder<'a>,
    spec: LayoutPaneSpec,
}

impl<'a> LayoutPaneBuilder<'a> {
    /// Runs the pane process directly as structured argv.
    ///
    /// On Windows, `.bat` and `.cmd` targets are rejected; use a native
    /// executable for literal argv, or [`Self::shell`] when shell
    /// interpretation is intentional.
    #[must_use]
    pub fn spawn<I, S>(mut self, command: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.spec.command = Some(ProcessCommandSpec::Argv(
            command.into_iter().map(Into::into).collect(),
        ));
        self
    }

    /// Runs pane command text through the configured shell.
    #[must_use]
    pub fn shell(mut self, command: impl Into<String>) -> Self {
        self.spec.command = Some(ProcessCommandSpec::Shell(command.into()));
        self
    }

    /// Sets the process working directory for this pane.
    #[must_use]
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.spec.cwd = Some(cwd.into());
        self
    }

    /// Adds one environment override for this pane process.
    #[must_use]
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.spec.env.push((key.into(), value.into()));
        self
    }

    /// Controls whether this pane remains visible after its process exits.
    #[must_use]
    pub const fn keep_alive_on_exit(mut self, keep_alive: bool) -> Self {
        self.spec.keep_alive_on_exit = Some(keep_alive);
        self
    }

    /// Starts another pane declaration after finalizing this one.
    #[must_use]
    pub fn pane(self, title: impl Into<String>) -> Self {
        self.finish().pane(title)
    }

    /// Applies the grid after finalizing this pane declaration.
    pub async fn apply(self) -> Result<PaneSet> {
        self.finish().apply().await
    }

    fn finish(mut self) -> GridLayoutBuilder<'a> {
        self.builder.panes.push(self.spec);
        self.builder
    }
}

#[derive(Debug, Clone)]
struct LayoutPaneSpec {
    title: String,
    command: Option<ProcessCommandSpec>,
    cwd: Option<PathBuf>,
    env: Vec<(String, String)>,
    keep_alive_on_exit: Option<bool>,
}

impl LayoutPaneSpec {
    fn new(title: String) -> Self {
        Self {
            title,
            command: None,
            cwd: None,
            env: Vec::new(),
            keep_alive_on_exit: None,
        }
    }
}

async fn apply_grid(builder: GridLayoutBuilder<'_>) -> Result<PaneSet> {
    let mut created_panes = Vec::new();
    let result = apply_grid_inner(builder, &mut created_panes).await;
    if result.is_err() {
        rollback_created_panes(created_panes).await;
    }
    result
}

async fn apply_grid_inner(
    builder: GridLayoutBuilder<'_>,
    created_panes: &mut Vec<Pane>,
) -> Result<PaneSet> {
    let capacity = validate_grid(builder.columns, builder.rows)?;
    validate_pane_count(builder.panes.len(), capacity)?;
    let session = builder.session.begin_operation_handle();
    let deadline = session
        .operation_deadline()
        .expect("operation session always carries a deadline scope");

    let window = session
        .window(builder.window_index)
        .with_operation_deadline(deadline);
    let mut existing = window.panes().await?;
    if existing.len() != 1 && builder.replace_existing_panes {
        close_extra_panes(&session, &existing).await?;
        existing = window.panes().await?;
    }
    if existing.len() != 1 {
        return Err(layout_error(format!(
            "layout builder expects exactly one existing pane in window {}; found {}. \
             Use replace_existing_panes(true) to close extras first",
            builder.window_index,
            existing.len()
        )));
    }

    let root_target = &existing[0].target;
    let root = session
        .pane(root_target.window_index, root_target.pane_index)
        .with_operation_deadline(deadline);
    let mut panes = vec![None; builder.panes.len()];

    let root_pane = configure_existing_root(
        &session,
        root,
        &builder.panes[0],
        builder.replace_existing_root_process,
    )
    .await?;
    panes[0] = Some(root_pane.clone());

    let row_count = row_count(builder.panes.len(), builder.columns);
    let mut row_anchors = Vec::with_capacity(row_count);
    row_anchors.push(root_pane);

    for row in 1..row_count {
        let spec_index = row * builder.columns;
        let anchor = row_anchors[row - 1].clone();
        let pane = split_new_pane(
            &session,
            &anchor,
            SplitDirection::Down,
            &builder.panes[spec_index],
        )
        .await?;
        panes[spec_index] = Some(pane.clone());
        created_panes.push(pane.clone());
        row_anchors.push(pane);
    }

    for (row, row_anchor) in row_anchors.iter().enumerate() {
        let row_start = row * builder.columns;
        let row_end = usize::min(row_start + builder.columns, builder.panes.len());
        let mut previous = row_anchor.clone();
        for (spec_index, slot) in panes
            .iter_mut()
            .enumerate()
            .take(row_end)
            .skip(row_start + 1)
        {
            let pane = split_new_pane(
                &session,
                &previous,
                SplitDirection::Right,
                &builder.panes[spec_index],
            )
            .await?;
            *slot = Some(pane.clone());
            created_panes.push(pane.clone());
            previous = pane;
        }
    }

    spread_window(&session, builder.window_index).await?;
    Ok(PaneSet::new(
        panes
            .into_iter()
            .map(|pane| pane.expect("every declared pane is created"))
            .collect::<Vec<_>>(),
    ))
}

async fn rollback_created_panes(mut panes: Vec<Pane>) {
    while let Some(pane) = panes.pop() {
        let _ = pane.close().await;
    }
}

async fn close_extra_panes(session: &Session, panes: &[crate::WindowPane]) -> Result<()> {
    let deadline = session
        .operation_deadline()
        .expect("layout cleanup runs inside an operation scope");
    for pane in panes.iter().skip(1).rev() {
        session
            .pane_by_id(pane.id)
            .await?
            .with_operation_deadline(deadline)
            .close()
            .await?;
    }
    Ok(())
}

async fn configure_existing_root(
    session: &Session,
    pane: Pane,
    spec: &LayoutPaneSpec,
    replace_existing: bool,
) -> Result<Pane> {
    match spec.command.clone() {
        Some(ProcessCommandSpec::Argv(argv)) => {
            let mut spawn = pane.spawn(argv).kill_existing(replace_existing);
            spawn = apply_spawn_options(spawn, spec);
            spawn.await?;
        }
        Some(ProcessCommandSpec::Shell(command)) => {
            let mut spawn = pane.shell(command).kill_existing(replace_existing);
            spawn = apply_spawn_options(spawn, spec);
            spawn.await?;
        }
        None => {
            validate_existing_root_options(spec)?;
            pane.set_title(spec.title.clone()).await?;
        }
    }

    stable_pane(session, &pane).await
}

fn apply_spawn_options<'a>(
    mut spawn: crate::PaneSpawnBuilder<'a>,
    spec: &LayoutPaneSpec,
) -> crate::PaneSpawnBuilder<'a> {
    if let Some(cwd) = spec.cwd.clone() {
        spawn = spawn.cwd(cwd);
    }
    for (key, value) in &spec.env {
        spawn = spawn.env(key.clone(), value.clone());
    }
    if let Some(keep_alive) = spec.keep_alive_on_exit {
        spawn = spawn.keep_alive_on_exit(keep_alive);
    }
    spawn.title(spec.title.clone())
}

async fn split_new_pane(
    session: &Session,
    anchor: &Pane,
    direction: SplitDirection,
    spec: &LayoutPaneSpec,
) -> Result<Pane> {
    let deadline = session
        .operation_deadline()
        .expect("layout split runs inside an operation scope");
    let anchor = anchor.with_operation_deadline(deadline);
    let mut split = anchor.split_with(direction);
    if let Some(cwd) = spec.cwd.clone() {
        split = split.cwd(cwd);
    }
    for (key, value) in &spec.env {
        split = split.env(key.clone(), value.clone());
    }
    if let Some(keep_alive) = spec.keep_alive_on_exit {
        split = split.keep_alive_on_exit(keep_alive);
    }
    split = match spec.command.clone() {
        Some(ProcessCommandSpec::Argv(argv)) => split.spawn(argv),
        Some(ProcessCommandSpec::Shell(command)) => split.shell(command),
        None => split,
    };
    split = split.title(spec.title.clone());

    let pane = split.await?;
    stable_pane(session, &pane).await
}

async fn stable_pane(session: &Session, pane: &Pane) -> Result<Pane> {
    let deadline = session
        .operation_deadline()
        .expect("layout identity lookup runs inside an operation scope");
    let pane_id = pane
        .with_operation_deadline(deadline)
        .id()
        .await?
        .ok_or_else(|| layout_error("created pane vanished before its id could be read"))?;
    session.pane_by_id(pane_id).await
}

async fn spread_window(session: &Session, window_index: u32) -> Result<()> {
    let target = WindowRef::new(session.name().clone(), window_index);
    match session
        .transport()
        .request(Request::SpreadLayout(SpreadLayoutRequest {
            target: SelectLayoutTarget::Window(target.to_proto()),
        }))
        .await?
    {
        Response::SelectLayout(_) => Ok(()),
        response => Err(unexpected_response("select-layout -E", response)),
    }
}

fn validate_existing_root_options(spec: &LayoutPaneSpec) -> Result<()> {
    if spec.cwd.is_some() || !spec.env.is_empty() || spec.keep_alive_on_exit.is_some() {
        return Err(layout_error(
            "cwd, env, and keep_alive_on_exit on the existing root pane require spawn() or shell()",
        ));
    }
    Ok(())
}

fn validate_grid(columns: usize, rows: usize) -> Result<usize> {
    if columns == 0 || rows == 0 {
        return Err(layout_error(
            "grid columns and rows must be greater than zero",
        ));
    }
    columns
        .checked_mul(rows)
        .ok_or_else(|| layout_error("grid dimensions overflow usize"))
}

fn validate_pane_count(count: usize, capacity: usize) -> Result<()> {
    if count == 0 {
        return Err(layout_error("layout must declare at least one pane"));
    }
    if count > capacity {
        return Err(layout_error(format!(
            "layout declares {count} panes but grid capacity is {capacity}"
        )));
    }
    Ok(())
}

fn row_count(pane_count: usize, columns: usize) -> usize {
    ((pane_count - 1) / columns) + 1
}

fn layout_error(message: impl Into<String>) -> RmuxError {
    RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
        "invalid layout builder request: {}",
        message.into()
    )))
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
