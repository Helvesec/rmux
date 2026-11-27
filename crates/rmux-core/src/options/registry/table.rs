use rmux_proto::OptionName;

use super::{DefaultValue, GlobalRoot, OptionChangeMask, OptionMetadata, OptionValueType};

mod server;
mod session;
mod session_status;
mod window;
mod window_display;

const ALLOW_PASSTHROUGH_CHOICES: &[&str] = &["off", "on", "all"];
const BELL_ACTION_CHOICES: &[&str] = &["none", "any", "current", "other"];
const CLOCK_MODE_STYLE_CHOICES: &[&str] = &["12", "24", "12-with-seconds", "24-with-seconds"];
const COPY_MODE_LINE_NUMBERS_CHOICES: &[&str] =
    &["off", "default", "absolute", "relative", "hybrid"];
const CURSOR_STYLE_CHOICES: &[&str] = &[
    "default",
    "blinking-block",
    "block",
    "blinking-underline",
    "underline",
    "blinking-bar",
    "bar",
];
const DESTROY_UNATTACHED_CHOICES: &[&str] = &["off", "on", "keep-last", "keep-group"];
const DETACH_ON_DESTROY_CHOICES: &[&str] = &["off", "on", "no-detached", "previous", "next"];
const EXTENDED_KEYS_CHOICES: &[&str] = &["off", "on", "always"];
const EXTENDED_KEYS_FORMAT_CHOICES: &[&str] = &["csi-u", "xterm"];
const GET_CLIPBOARD_CHOICES: &[&str] = &["off", "buffer", "request", "both"];
const MESSAGE_LINE_CHOICES: &[&str] = &["0", "1", "2", "3", "4"];
const MODE_KEYS_CHOICES: &[&str] = &["emacs", "vi"];
const PANE_BORDER_INDICATORS_CHOICES: &[&str] = &["off", "colour", "arrows", "both"];
const PANE_BORDER_LINES_CHOICES: &[&str] =
    &["single", "double", "heavy", "simple", "number", "spaces"];
const PANE_SCROLLBARS_CHOICES: &[&str] = &["off", "modal", "on"];
const PANE_SCROLLBARS_POSITION_CHOICES: &[&str] = &["right", "left"];
const PANE_STATUS_CHOICES: &[&str] = &["off", "top", "bottom"];
const POPUP_BORDER_LINES_CHOICES: &[&str] = &[
    "single", "double", "heavy", "simple", "rounded", "padded", "none",
];
const REMAIN_ON_EXIT_CHOICES: &[&str] = &["off", "on", "failed", "key"];
const SET_CLIPBOARD_CHOICES: &[&str] = &["off", "external", "on"];
const STATUS_CHOICES: &[&str] = &["off", "on", "2", "3", "4", "5"];
const STATUS_JUSTIFY_CHOICES: &[&str] = &["left", "centre", "right", "absolute-centre"];
const STATUS_KEYS_CHOICES: &[&str] = &["emacs", "vi"];
const STATUS_POSITION_CHOICES: &[&str] = &["top", "bottom"];
const VISUAL_BELL_CHOICES: &[&str] = &["off", "on", "both"];
const WINDOW_SIZE_CHOICES: &[&str] = &["largest", "smallest", "manual", "latest"];
const STATUS_FORMAT0: &str = "#[align=left range=left #{E:status-left-style}]#[push-default]#{T;=/#{status-left-length}:status-left}#[pop-default]#[norange default]#[list=on align=#{status-justify}]#[list=left-marker]<#[list=right-marker]>#[list=on]#{W:#[range=window|#{window_index} #{E:window-status-style}#{?#{&&:#{window_last_flag},#{!=:#{E:window-status-last-style},default}}, #{E:window-status-last-style},}#{?#{&&:#{window_bell_flag},#{!=:#{E:window-status-bell-style},default}}, #{E:window-status-bell-style},#{?#{&&:#{||:#{window_activity_flag},#{window_silence_flag}},#{!=:#{E:window-status-activity-style},default}}, #{E:window-status-activity-style},}}]#[push-default]#{T:window-status-format}#[pop-default]#[norange default]#{?loop_last_flag,,#{E:window-status-separator}},#[range=window|#{window_index} list=focus #{?#{!=:#{E:window-status-current-style},default},#{E:window-status-current-style},#{E:window-status-style}}#{?#{&&:#{window_last_flag},#{!=:#{E:window-status-last-style},default}}, #{E:window-status-last-style},}#{?#{&&:#{window_bell_flag},#{!=:#{E:window-status-bell-style},default}}, #{E:window-status-bell-style},#{?#{&&:#{||:#{window_activity_flag},#{window_silence_flag}},#{!=:#{E:window-status-activity-style},default}}, #{E:window-status-activity-style},}}]#[push-default]#{T:window-status-current-format}#[pop-default]#[norange list=on default]#{?loop_last_flag,,#{E:window-status-separator}}}#[nolist align=right range=right #{E:status-right-style}]#[push-default]#{T;=/#{status-right-length}:status-right}#[pop-default]#[norange default]";
const STATUS_FORMAT1: &str = "#[align=left]#{R: ,#{n:#{session_name}}}P: #[norange default]#[list=on align=#{status-justify}]#[list=left-marker]<#[list=right-marker]>#[list=on]#{P:#[range=pane|#{pane_id} #{E:pane-status-style}]#[push-default]#{T:window-pane-status-format}#[pop-default]#[norange list=on default]  ,#[range=pane|#{pane_id} list=focus #{?#{!=:#{E:pane-status-current-style},default},#{E:pane-status-current-style},#{E:pane-status-style}}]#[push-default]#{T:window-pane-current-status-format}#[pop-default]#[norange list=on default] }";
const STATUS_FORMAT2: &str = "#[align=left]#{R: ,#{n:#{session_name}}}S: #[norange default]#[list=on align=#{status-justify}]#[list=left-marker]<#[list=right-marker]>#[list=on]#{S:#[range=session|#{session_id} #{E:session-status-style}]#[push-default]#S#{session_alert}#[pop-default]#[norange list=on default]  ,#[range=session|#{session_id} list=focus #{?#{!=:#{E:session-status-current-style},default},#{E:session-status-current-style},#{E:session-status-style}}]#[push-default]#S*#{session_alert}#[pop-default]#[norange list=on default] }";
const STATUS_FORMAT_DEFAULT: &[&str] = &[STATUS_FORMAT0, STATUS_FORMAT1, STATUS_FORMAT2];
#[cfg(unix)]
const DEFAULT_SHELL: &str = "";
#[cfg(windows)]
const DEFAULT_SHELL: &str = "";

// The sections keep the `options-table.c` order; they are concatenated at
// compile time into the single registry slice.
const OPTION_COUNT: usize = server::OPTIONS.len()
    + session::OPTIONS.len()
    + session_status::OPTIONS.len()
    + window::OPTIONS.len()
    + window_display::OPTIONS.len();

pub(super) const OPTIONS: &[OptionMetadata] = &concat_sections::<OPTION_COUNT>([
    server::OPTIONS,
    session::OPTIONS,
    session_status::OPTIONS,
    window::OPTIONS,
    window_display::OPTIONS,
]);

#[allow(clippy::too_many_arguments)]
const fn option(
    option: OptionName,
    name: &'static str,
    aliases: &'static [&'static str],
    show_mask: u8,
    scope_mask: u8,
    global_root: GlobalRoot,
    value_type: OptionValueType,
    default: DefaultValue,
    separator: &'static str,
    is_array: bool,
    effects: OptionChangeMask,
) -> OptionMetadata {
    OptionMetadata {
        wire_option: Some(option),
        name,
        aliases,
        show_mask,
        scope_mask,
        global_root,
        value_type,
        default,
        separator,
        is_array,
        effects,
    }
}

/// Copies the sections, in order, into one array of `N` entries.
const fn concat_sections<const N: usize>(sections: [&[OptionMetadata]; 5]) -> [OptionMetadata; N] {
    let mut table = [sections[0][0]; N];
    let mut filled = 0;
    let mut section = 0;
    while section < sections.len() {
        let mut entry = 0;
        while entry < sections[section].len() {
            table[filled] = sections[section][entry];
            filled += 1;
            entry += 1;
        }
        section += 1;
    }
    assert!(filled == N);
    table
}
