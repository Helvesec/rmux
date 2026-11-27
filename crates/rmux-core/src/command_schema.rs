//! Canonical public command schema. Argument types and parse policies are
//! expanded in the CLI crate; the core consumes only lookup and usage metadata.
//! The declaration order is independent of the stable inventory order.

/// Expands the public command schema into a consumer-defined projection.
///
/// Consumers receive typed commands, delegated commands, and expansion aliases.
/// Argument types refer to the CLI model; core projections need not resolve them.
#[macro_export]
macro_rules! command_schema {
    ($consumer:ident) => {
        $consumer! {
            commands {
                ("attach-session", Some("attach"), tmux, 0,
                    "(attach) [-dErx] [-c working-directory] [-f flags] [-t target-session]",
                    AttachSession(AttachSessionArgs), AttachSessionArgs, [plain]);
                ("bind-key", Some("bind"), tmux, 1,
                    "(bind) [-nr] [-T key-table] [-N note] key [command [argument ...]]",
                    BindKey(BindKeyArgs), BindKeyArgs, [plain]);
                ("break-pane", Some("breakp"), tmux, 2,
                    "(breakp) [-abdP] [-F format] [-n window-name] [-s src-pane] [-t dst-window]",
                    BreakPane(BreakPaneArgs), BreakPaneArgs, [plain]);
                ("capture-pane", Some("capturep"), tmux, 3,
                    "(capturep) [-aCeFHJLMNpPqT] [-b buffer-name] [-E end-line] [-S start-line] [-t target-pane]",
                    CapturePane(CapturePaneArgs), CapturePaneArgs, [validate]);
                ("choose-buffer", None, tmux, 4,
                    "[-NrZ] [-F format] [-f filter] [-K key-format] [-O sort-order] [-t target-pane] [template]",
                    ChooseBuffer(ChooseBufferArgs), ChooseBufferArgs, [queued_validate]);
                ("choose-client", None, tmux, 5,
                    "[-NrZ] [-F format] [-f filter] [-K key-format] [-O sort-order] [-t target-pane] [template]",
                    ChooseClient(ChooseClientArgs), ChooseClientArgs, [queued_validate]);
                ("choose-tree", None, tmux, 6,
                    "[-GNrswZ] [-F format] [-f filter] [-K key-format] [-O sort-order] [-t target-pane] [template]",
                    ChooseTree(ChooseTreeArgs), ChooseTreeArgs, [queued_validate]);
                ("clear-history", Some("clearhist"), tmux, 7,
                    "(clearhist) [-H] [-t target-pane]",
                    ClearHistory(ClearHistoryArgs), ClearHistoryArgs, [plain]);
                ("clear-prompt-history", Some("clearphist"), tmux, 8,
                    "(clearphist) [-T prompt-type]",
                    ClearPromptHistory(PromptHistoryArgs), PromptHistoryArgs, [queued]);
                ("clock-mode", None, tmux, 9,
                    "[-t target-pane]",
                    ClockMode(ClockModeArgs), ClockModeArgs, [plain]);
                ("command-prompt", None, tmux, 10,
                    "[-1CbeFiklN] [-I inputs] [-p prompts] [-t target-client] [-T prompt-type] [template]",
                    Prompt(PromptArgs), PromptArgs, [queued_validate]);
                ("confirm-before", Some("confirm"), tmux, 11,
                    "(confirm) [-by] [-c confirm-key] [-p prompt] [-t target-client] command",
                    ConfirmBefore(ConfirmBeforeArgs), ConfirmBeforeArgs, [queued]);
                ("copy-mode", None, tmux, 12,
                    "[-deHMqSu] [-s src-pane] [-t target-pane]",
                    CopyMode(CopyModeArgs), CopyModeArgs, [validate]);
                ("customize-mode", None, tmux, 13,
                    "[-NZ] [-F format] [-f filter] [-t target-pane]",
                    CustomizeMode(CustomizeModeArgs), CustomizeModeArgs, [queued]);
                ("delete-buffer", Some("deleteb"), tmux, 14,
                    "(deleteb) [-b buffer-name]",
                    DeleteBuffer(DeleteBufferArgs), DeleteBufferArgs, [plain]);
                ("detach-client", Some("detach"), tmux, 15,
                    "(detach) [-aP] [-E shell-command] [-s target-session] [-t target-client]",
                    DetachClient(DetachClientArgs), DetachClientArgs, [plain]);
                ("display-menu", Some("menu"), tmux, 16,
                    "(menu) [-MO] [-b border-lines] [-c target-client] [-C starting-choice] [-H selected-style] [-s style] [-S border-style] [-t target-pane] [-T title] [-x position] [-y position] name [key] [command] ...",
                    DisplayMenu(DisplayMenuArgs), DisplayMenuArgs, [queued]);
                ("display-message", Some("display"), tmux, 17,
                    "(display) [-aCIlNpv] [-c target-client] [-d delay] [-F format] [-t target-pane] [message]",
                    DisplayMessage(DisplayMessageArgs), DisplayMessageArgs, [queued_validate]);
                ("display-popup", Some("popup"), tmux, 18,
                    "(popup) [-BCEkN] [-b border-lines] [-c target-client] [-d start-directory] [-e environment] [-h height] [-s style] [-S border-style] [-t target-pane] [-T title] [-w width] [-x position] [-y position] [shell-command [argument ...]]",
                    DisplayPopup(DisplayPopupArgs), DisplayPopupArgs, [queued]);
                ("display-panes", Some("displayp"), tmux, 19,
                    "(displayp) [-bN] [-d duration] [-t target-client] [template]",
                    DisplayPanes(DisplayPanesArgs), DisplayPanesArgs, [plain]);
                ("find-window", Some("findw"), tmux, 20,
                    "(findw) [-CiNrTZ] [-t target-pane] match-string",
                    FindWindow(FindWindowArgs), FindWindowArgs, [queued]);
                ("has-session", Some("has"), tmux, 21,
                    "(has) [-t target-session]",
                    HasSession(HasSessionArgs), HasSessionArgs, [plain]);
                ("if-shell", Some("if"), tmux, 22,
                    "(if) [-bF] [-t target-pane] shell-command command [command]",
                    IfShell(IfShellArgs), IfShellArgs, [queued]);
                ("join-pane", Some("joinp"), tmux, 23,
                    "(joinp) [-bdfhv] [-l size] [-s src-pane] [-t dst-pane]",
                    JoinPane(JoinPaneArgs), JoinPaneArgs, [named parse_join_pane_args]);
                ("kill-pane", Some("killp"), tmux, 24,
                    "(killp) [-a] [-t target-pane]",
                    KillPane(PaneTargetArgs), PaneTargetArgs, [plain]);
                ("kill-server", None, tmux, 25,
                    "",
                    KillServer(), EmptyArgs, [empty]);
                ("kill-session", None, tmux, 26,
                    "[-aCg] [-t target-session]",
                    KillSession(KillSessionArgs), KillSessionArgs, [plain]);
                ("kill-window", Some("killw"), tmux, 27,
                    "(killw) [-a] [-t target-window]",
                    KillWindow(KillWindowArgs), KillWindowArgs, [plain]);
                ("last-pane", Some("lastp"), tmux, 28,
                    "(lastp) [-deZ] [-t target-window]",
                    LastPane(LastPaneArgs), LastPaneArgs, [plain]);
                ("last-window", Some("last"), tmux, 29,
                    "(last) [-t target-session]",
                    LastWindow(SessionTargetArgs), SessionTargetArgs, [plain]);
                ("link-window", Some("linkw"), tmux, 30,
                    "(linkw) [-abdk] [-s src-window] [-t dst-window]",
                    LinkWindow(LinkWindowArgs), LinkWindowArgs, [plain]);
                ("list-buffers", Some("lsb"), tmux, 31,
                    "(lsb) [-F format] [-f filter] [-O order]",
                    ListBuffers(ListBuffersArgs), ListBuffersArgs, [plain]);
                ("list-clients", Some("lsc"), tmux, 32,
                    "(lsc) [-F format] [-f filter] [-O order][-t target-session]",
                    ListClients(ListClientsArgs), ListClientsArgs, [plain]);
                ("list-commands", Some("lscm"), tmux, 33,
                    "(lscm) [-F format] [command]",
                    ListCommands(ListCommandsArgs), ListCommandsArgs, [plain]);
                ("list-keys", Some("lsk"), tmux, 34,
                    "(lsk) [-1aNr] [-F format] [-O order] [-P prefix-string][-T key-table] [key]",
                    ListKeys(ListKeysArgs), ListKeysArgs, [validate]);
                ("list-panes", Some("lsp"), tmux, 35,
                    "(lsp) [-asr] [-F format] [-f filter] [-O order][-t target-window]",
                    ListPanes(ListPanesArgs), ListPanesArgs, [plain]);
                ("list-sessions", Some("ls"), tmux, 36,
                    "(ls) [-r] [-F format] [-f filter] [-O order]",
                    ListSessions(ListSessionsArgs), ListSessionsArgs, [validate]);
                ("list-windows", Some("lsw"), tmux, 37,
                    "(lsw) [-ar] [-F format] [-f filter] [-O order][-t target-session]",
                    ListWindows(ListWindowsArgs), ListWindowsArgs, [plain]);
                ("load-buffer", Some("loadb"), tmux, 38,
                    "(loadb) [-b buffer-name] [-t target-client] path",
                    LoadBuffer(LoadBufferArgs), LoadBufferArgs, [plain]);
                ("lock-client", Some("lockc"), tmux, 39,
                    "(lockc) [-t target-client]",
                    LockClient(ClientTargetArgs), ClientTargetArgs, [plain]);
                ("lock-server", Some("lock"), tmux, 40,
                    "(lock) ",
                    LockServer(), EmptyArgs, [empty]);
                ("lock-session", Some("locks"), tmux, 41,
                    "(locks) [-t target-session]",
                    LockSession(SessionTargetArgs), SessionTargetArgs, [plain]);
                ("move-pane", Some("movep"), tmux, 42,
                    "(movep) [-bdfhv] [-l size] [-s src-pane] [-t dst-pane]",
                    MovePane(JoinPaneArgs), JoinPaneArgs, [named parse_join_pane_args]);
                ("move-window", Some("movew"), tmux, 43,
                    "(movew) [-abdkr] [-s src-window] [-t dst-window]",
                    MoveWindow(MoveWindowArgs), MoveWindowArgs, [plain]);
                ("new-session", Some("new"), tmux, 45,
                    "(new) [-AdDEPX] [-c start-directory] [-e environment] [-F format] [-f flags] [-n window-name] [-s session-name] [-t target-session] [-x width] [-y height] [shell-command [argument ...]]",
                    NewSession(NewSessionArgs), NewSessionArgs, [plain]);
                ("new-window", Some("neww"), tmux, 46,
                    "(neww) [-abdkPS] [-c start-directory] [-e environment] [-F format] [-n window-name] [-t target-window] [shell-command [argument ...]]",
                    NewWindow(NewWindowArgs), NewWindowArgs, [queued]);
                ("next-layout", Some("nextl"), tmux, 47,
                    "(nextl) [-t target-window]",
                    NextLayout(WindowTargetArgs), WindowTargetArgs, [plain]);
                ("next-window", Some("next"), tmux, 48,
                    "(next) [-a] [-t target-session]",
                    NextWindow(AlertSessionTargetArgs), AlertSessionTargetArgs, [plain]);
                ("paste-buffer", Some("pasteb"), tmux, 49,
                    "(pasteb) [-dprS] [-s separator] [-b buffer-name] [-t target-pane]",
                    PasteBuffer(PasteBufferArgs), PasteBufferArgs, [plain]);
                ("pipe-pane", Some("pipep"), tmux, 50,
                    "(pipep) [-IOo] [-t target-pane] [shell-command]",
                    PipePane(PipePaneArgs), PipePaneArgs, [plain]);
                ("previous-layout", Some("prevl"), tmux, 51,
                    "(prevl) [-t target-window]",
                    PreviousLayout(WindowTargetArgs), WindowTargetArgs, [plain]);
                ("previous-window", Some("prev"), tmux, 52,
                    "(prev) [-a] [-t target-session]",
                    PreviousWindow(AlertSessionTargetArgs), AlertSessionTargetArgs, [plain]);
                ("refresh-client", Some("refresh"), tmux, 53,
                    "(refresh) [-cDLRUlS] [-A pane:state] [-B name:what:format] [-C size] [-f flags] [-F flags] [-r pane:report] [-t target-client] [adjustment]",
                    RefreshClient(RefreshClientArgs), RefreshClientArgs, [validate]);
                ("rename-session", Some("rename"), tmux, 54,
                    "(rename) [-t target-session] new-name",
                    RenameSession(RenameSessionArgs), RenameSessionArgs, [plain]);
                ("rename-window", Some("renamew"), tmux, 55,
                    "(renamew) [-t target-window] new-name",
                    RenameWindow(RenameWindowArgs), RenameWindowArgs, [custom parse_rename_window_args]);
                ("resize-pane", Some("resizep"), tmux, 56,
                    "(resizep) [-DLMRTUZ] [-x width] [-y height] [-X x] [-Y y] [-t target-pane] [adjustment]",
                    ResizePane(ResizePaneArgs), ResizePaneArgs, [custom parse_resize_pane_args]);
                ("resize-window", Some("resizew"), tmux, 57,
                    "(resizew) [-aADLRU] [-x width] [-y height] [-t target-window] [adjustment]",
                    ResizeWindow(ResizeWindowArgs), ResizeWindowArgs, [plain]);
                ("respawn-pane", Some("respawnp"), tmux, 58,
                    "(respawnp) [-k] [-c start-directory] [-e environment] [-t target-pane] [shell-command [argument ...]]",
                    RespawnPane(RespawnPaneArgs), RespawnPaneArgs, [plain]);
                ("respawn-window", Some("respawnw"), tmux, 59,
                    "(respawnw) [-k] [-c start-directory] [-e environment] [-t target-window] [shell-command [argument ...]]",
                    RespawnWindow(RespawnWindowArgs), RespawnWindowArgs, [plain]);
                ("rotate-window", Some("rotatew"), tmux, 60,
                    "(rotatew) [-DUZ] [-t target-window]",
                    RotateWindow(RotateWindowArgs), RotateWindowArgs, [plain]);
                ("run-shell", Some("run"), tmux, 61,
                    "(run) [-bCE] [-c start-directory] [-d delay] [-t target-pane] [shell-command [argument ...]]",
                    RunShell(RunShellArgs), RunShellArgs, [queued_validate]);
                ("save-buffer", Some("saveb"), tmux, 62,
                    "(saveb) [-a] [-b buffer-name] path",
                    SaveBuffer(SaveBufferArgs), SaveBufferArgs, [plain]);
                ("select-layout", Some("selectl"), tmux, 63,
                    "(selectl) [-Enop] [-t target-pane] [layout-name]",
                    SelectLayout(SelectLayoutArgs), SelectLayoutArgs, [custom parse_select_layout_args]);
                ("select-pane", Some("selectp"), tmux, 64,
                    "(selectp) [-DdeLlMmRUZ] [-T title] [-t target-pane]",
                    SelectPane(SelectPaneArgs), SelectPaneArgs, [custom parse_select_pane_args]);
                ("select-window", Some("selectw"), tmux, 65,
                    "(selectw) [-lnpT] [-t target-window]",
                    SelectWindow(SelectWindowArgs), SelectWindowArgs, [custom parse_select_window_args]);
                ("send-keys", Some("send"), tmux, 66,
                    "(send) [-FHKlMRX] [-c target-client] [-N repeat-count] [-t target-pane] [key ...]",
                    SendKeys(SendKeysArgs), SendKeysArgs, [custom parse_send_keys_args]);
                ("send-prefix", None, tmux, 67,
                    "[-2] [-t target-pane]",
                    SendPrefix(SendPrefixArgs), SendPrefixArgs, [plain]);
                ("server-access", None, tmux, 68,
                    "[-adlrw] [user]",
                    ServerAccess(ServerAccessArgs), ServerAccessArgs, [custom parse_server_access_args]);
                ("set-buffer", Some("setb"), tmux, 69,
                    "(setb) [-aw] [-b buffer-name] [-n new-buffer-name] [-t target-client] [data]",
                    SetBuffer(SetBufferArgs), SetBufferArgs, [custom parse_set_buffer_args]);
                ("set-environment", Some("setenv"), tmux, 70,
                    "(setenv) [-Fhgru] [-t target-session] variable [value]",
                    SetEnvironment(SetEnvironmentArgs), SetEnvironmentArgs, [plain]);
                ("set-hook", None, tmux, 71,
                    "[-agpRuw] [-t target-pane] hook [command]",
                    SetHook(SetHookArgs), SetHookArgs, [plain]);
                ("set-option", Some("set"), tmux, 72,
                    "(set) [-aFgopqsuUw] [-t target-pane] option [value]",
                    SetOption(SetOptionArgs), SetOptionArgs, [set_option SetOptionCommandKind::SetOption]);
                ("set-window-option", Some("setw"), tmux, 73,
                    "(setw) [-aFgoqu] [-t target-window] option [value]",
                    SetWindowOption(SetOptionArgs), SetWindowOptionArgs, [set_option SetOptionCommandKind::SetWindowOption]);
                ("show-buffer", Some("showb"), tmux, 74,
                    "(showb) [-b buffer-name]",
                    ShowBuffer(ShowBufferArgs), ShowBufferArgs, [plain]);
                ("show-environment", Some("showenv"), tmux, 75,
                    "(showenv) [-hgs] [-t target-session] [variable]",
                    ShowEnvironment(ShowEnvironmentArgs), ShowEnvironmentArgs, [plain]);
                ("show-hooks", None, tmux, 76,
                    "[-gpw] [-t target-pane] [hook]",
                    ShowHooks(ShowHooksArgs), ShowHooksArgs, [plain]);
                ("show-messages", Some("showmsgs"), tmux, 77,
                    "(showmsgs) [-JT] [-t target-client]",
                    ShowMessages(ShowMessagesArgs), ShowMessagesArgs, [plain]);
                ("show-options", Some("show"), tmux, 78,
                    "(show) [-AgHpqsvw] [-t target-pane] [option]",
                    ShowOptions(ShowOptionsArgs), ShowOptionsArgs, [plain]);
                ("show-prompt-history", Some("showphist"), tmux, 79,
                    "(showphist) [-T prompt-type]",
                    ShowPromptHistory(PromptHistoryArgs), PromptHistoryArgs, [queued]);
                ("show-window-options", Some("showw"), tmux, 80,
                    "(showw) [-gv] [-t target-window] [option]",
                    ShowWindowOptions(ShowOptionsArgs), ShowWindowOptionsArgs, [converted]);
                ("source-file", Some("source"), tmux, 81,
                    "(source) [-Fnqv] [-t target-pane] path ...",
                    SourceFile(SourceFileArgs), SourceFileArgs, [custom parse_source_file_args]);
                ("split-window", Some("splitw"), tmux, 82,
                    "(splitw) [-bdefhIklPvZ] [-c start-directory] [-e environment] [-F format] [-l size] [-p percentage] [-t target-pane] [shell-command [argument ...]]",
                    SplitWindow(SplitWindowArgs), SplitWindowArgs, [custom parse_split_window_args]);
                ("start-server", Some("start"), tmux, 83,
                    "(start) ",
                    StartServer(StartServerArgs), StartServerArgs, [plain]);
                ("suspend-client", Some("suspendc"), tmux, 84,
                    "(suspendc) [-t target-client]",
                    SuspendClient(SuspendClientArgs), SuspendClientArgs, [plain]);
                ("swap-pane", Some("swapp"), tmux, 85,
                    "(swapp) [-dDUZ] [-s src-pane] [-t dst-pane]",
                    SwapPane(SwapPaneArgs), SwapPaneArgs, [plain]);
                ("swap-window", Some("swapw"), tmux, 86,
                    "(swapw) [-d] [-s src-window] [-t dst-window]",
                    SwapWindow(SwapWindowArgs), SwapWindowArgs, [custom parse_swap_window_args]);
                ("switch-client", Some("switchc"), tmux, 87,
                    "(switchc) [-ElnprZ] [-c target-client] [-t target-session] [-T key-table] [-O order]",
                    SwitchClient(SwitchClientArgs), SwitchClientArgs, [plain]);
                ("unbind-key", Some("unbind"), tmux, 88,
                    "(unbind) [-anq] [-T key-table] key",
                    UnbindKey(UnbindKeyArgs), UnbindKeyArgs, [plain]);
                ("unlink-window", Some("unlinkw"), tmux, 89,
                    "(unlinkw) [-k] [-t target-window]",
                    UnlinkWindow(UnlinkWindowArgs), UnlinkWindowArgs, [plain]);
                ("wait-for", Some("wait"), tmux, 90,
                    "(wait) [-L|-S|-U] channel",
                    WaitFor(WaitForArgs), WaitForArgs, [plain]);
                ("wait-pane", None, extension, 4,
                    "[-t target-pane] [--text text|--next-text text|--visible-text text|--quiet|--pane-exit|--get-by-text text] [--stable-for duration] [--timeout duration] [--json]",
                    WaitPane(WaitPaneArgs), WaitPaneArgs, [validate]);
                ("pane-snapshot", None, extension, 5,
                    "[-t target-pane] [--json] [--style] [--region row,col,rows,cols]",
                    PaneSnapshot(PaneSnapshotArgs), PaneSnapshotArgs, [plain]);
                ("export-window-snapshot-v2", None, extension, 6,
                    "[-t target-window]",
                    ExportWindowSnapshotV2(ExportWindowSnapshotArgs), ExportWindowSnapshotArgs, [plain]);
                ("restore-window-snapshot-v2", None, extension, 7,
                    "[-t target-window] snapshot-file",
                    RestoreWindowSnapshotV2(RestoreWindowSnapshotArgs), RestoreWindowSnapshotArgs, [plain]);
                ("export-window-snapshot", None, extension, 8,
                    "[-t target-window]",
                    ExportWindowSnapshot(ExportWindowSnapshotArgs), ExportWindowSnapshotArgs, [plain]);
                ("restore-window-snapshot", None, extension, 9,
                    "[-t target-window] snapshot-file",
                    RestoreWindowSnapshot(RestoreWindowSnapshotArgs), RestoreWindowSnapshotArgs, [plain]);
                ("stream-pane", None, extension, 10,
                    "[-t target-pane] [--raw|--lines]",
                    StreamPane(StreamPaneArgs), StreamPaneArgs, [validate]);
                ("collect-pane-output", None, extension, 11,
                    "[-t target-pane] --until-pane-exit --max-bytes bytes [--json]",
                    CollectPaneOutput(CollectPaneOutputArgs), CollectPaneOutputArgs, [validate]);
                ("locator", None, extension, 12,
                    "[-t target-pane] --get-by-text text [--json]",
                    Locator(LocatorArgs), LocatorArgs, [validate]);
                ("expect-pane", None, extension, 13,
                    "[-t target-pane] --get-by-text text [--visible|--hidden|--count count] [--json]",
                    ExpectPane(ExpectPaneArgs), ExpectPaneArgs, [validate]);
                ("find-panes", None, extension, 14,
                    "[--title title] [--title-prefix prefix] [--current-command command] [--cwd path] [--json]",
                    FindPanes(FindPanesArgs), FindPanesArgs, [plain]);
                ("find-sessions", None, extension, 15,
                    "[--name name] [--name-prefix prefix] [--json]",
                    FindSessions(FindSessionsArgs), FindSessionsArgs, [plain]);
                ("broadcast-keys", None, extension, 16,
                    "-t target-pane... [-l] -- key ...",
                    BroadcastKeys(BroadcastKeysArgs), BroadcastKeysArgs, [validate]);
                ("with-session", None, extension, 17,
                    "session-name [--kill-on-owner-exit] [--ttl duration] -- command ...",
                    WithSession(WithSessionArgs), WithSessionArgs, [interspersed]);
                ("web-share", None, extension, 18,
                    "[-lX] [-K share-id] [disconnect share-id] [--config] [--lookup share-id] [--operator-only|--spectator-only] [--ttl seconds|--expires-at RFC3339] [--kill-session-on-expire] [--max-operators count] [--max-spectators count] [--frontend-url url] [--tunnel-url url|--tunnel-provider provider] [--no-navbar] [--no-disclaimer] [--hide-viewers] [--theme user|light|dark] [--no-pin] [-t pane|session]",
                    WebShare(WebShareArgs), WebShareArgs, [custom super::web::parse_web_share_args]);
            }
            delegated {
                ("new-pane", Some("newp"), tmux, 44,
                    "(newp) [-dEIkP] [-c start-directory] [-e environment] [-F format] [-t target-pane] [-x width] [-y height] [-X x] [-Y y] [shell-command [argument ...]]",
                    negotiated);
                ("capabilities", None, extension, 0,
                    "[--human|--json]",
                    capabilities);
                ("claude", None, extension, 1,
                    "[install-skill|claude-args...]",
                    claude);
                ("doctor", None, extension, 2,
                    "tmux-dropin",
                    doctor);
                ("setup", None, extension, 3,
                    "tmux-shim",
                    setup);
                ("set-pane-placement", None, extension, 19,
                    "[-bhv] [-a tiled-neighbor] [-t target-pane] floating|tiled",
                    negotiated);
            }
            local {
                ("diagnose", DiagnoseArgs, "[--human|--json]");
            }
            aliases {
                ("choose-session", "choose-tree -s", ChooseTreeArgs);
                ("choose-window", "choose-tree -w", ChooseTreeArgs);
            }
        }
    };
}

pub(crate) mod metadata;
