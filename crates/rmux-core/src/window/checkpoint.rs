use super::*;

/// Immutable undo data, never a registered or independently mutable window.
pub(crate) struct WindowCheckpoint {
    panes: Vec<Pane>,
    next_pane_index: u32,
    active_pane: u32,
    last_pane: Option<u32>,
    layout: LayoutName,
    last_layout: Option<LayoutName>,
    layout_tree: Option<LayoutTree>,
    floating_stack: FloatingStack,
    resize_topology: rmux_proto::RmuxWindowResizeTopology,
    custom_layout: bool,
    old_layout: Option<String>,
    size: TerminalSize,
    manual_size: super::ManualWindowSize,
    name: Option<String>,
    automatic_rename: bool,
    zoomed: bool,
    zoom_restore_pending: bool,
    alert_flags: AlertFlags,
    alerts_queued: bool,
    created_at: i64,
    activity_at: i64,
    requested_main_width: Option<u16>,
    requested_main_height: Option<u16>,
    tiled_max_columns: Option<u16>,
}

impl Window {
    pub(crate) fn checkpoint(&self) -> WindowCheckpoint {
        WindowCheckpoint {
            panes: self.panes.clone(),
            next_pane_index: self.next_pane_index,
            active_pane: self.active_pane,
            last_pane: self.last_pane,
            layout: self.layout,
            last_layout: self.last_layout,
            layout_tree: self.current_layout_tree().map(|tree| tree.into_owned()),
            floating_stack: self.current_floating_stack().into_owned(),
            resize_topology: self.current_resize_topology(),
            custom_layout: self.custom_layout,
            old_layout: self.old_layout.clone(),
            size: self.size,
            manual_size: self.manual_size,
            name: self.name.clone(),
            automatic_rename: self.automatic_rename,
            zoomed: self.zoomed,
            zoom_restore_pending: self.zoom_restore_pending,
            alert_flags: self.alert_flags,
            alerts_queued: self.alerts_queued,
            created_at: self.created_at,
            activity_at: self.activity_at,
            requested_main_width: self.requested_main_width,
            requested_main_height: self.requested_main_height,
            tiled_max_columns: self.tiled_max_columns,
        }
    }

    pub(crate) fn restore_checkpoint(&mut self, checkpoint: WindowCheckpoint) {
        self.borrowed_pane_owners = None;
        self.panes = checkpoint.panes;
        self.next_pane_index = checkpoint.next_pane_index;
        self.active_pane = checkpoint.active_pane;
        self.last_pane = checkpoint.last_pane;
        self.layout = checkpoint.layout;
        self.last_layout = checkpoint.last_layout;
        self.layout_tree = checkpoint.layout_tree;
        self.floating_stack = checkpoint.floating_stack;
        self.resize_topology = checkpoint.resize_topology;
        self.custom_layout = checkpoint.custom_layout;
        self.old_layout = checkpoint.old_layout;
        self.size = checkpoint.size;
        self.manual_size = checkpoint.manual_size;
        self.name = checkpoint.name;
        self.automatic_rename = checkpoint.automatic_rename;
        self.zoomed = checkpoint.zoomed;
        self.zoom_restore_pending = checkpoint.zoom_restore_pending;
        self.alert_flags = checkpoint.alert_flags;
        self.alerts_queued = checkpoint.alerts_queued;
        self.created_at = checkpoint.created_at;
        self.activity_at = checkpoint.activity_at;
        self.requested_main_width = checkpoint.requested_main_width;
        self.requested_main_height = checkpoint.requested_main_height;
        self.tiled_max_columns = checkpoint.tiled_max_columns;
    }
}
