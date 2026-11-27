"""FAIL655: neutral descendants must retain the proved relocation transport."""
import copy
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

from model import borrow_objects, git, read_tree
from relocation_chain import EPOCH, admitted_semantics, load_chain, semantic_identities, transport


# D573(1)-(2): append-only population fixture epoch, based on c4541908.
# These are synthetic unit reports, preserving the D161 semantic fixture.
# Counts/physical lines come from the real scan; semantic slots are intentionally
# empty. Full source semantics (including ProcessChild) remain in the unchanged
# real-scan test and sealed D570-f7f744fb epoch, never certified by these fixtures.
# Each tuple binds a listed path to its first-parent introduction commit.
POPULATION_EPOCH = {
    'epoch': 'D573-c4541908-population',
    'revision': 'c454190854c82ee2bdb77bbcd01693c9bbed05d1',
    'decision': 'D573(1)-(3), 2026-11-03T14:48:02Z',
    'historical_population': 2312,
    'population': 2486,
    'semantic_transition': {
        'epoch': 'D570-f7f744fb',
        'sha256': 'dda052e58b0fb6a475909ac734967f099237f46be01c4b0dd31b3ec391da56c1',
        'admission': 'DECLARED_D573_2; no independent PASS or credit',
        'additions': 4,
        'removals': 3,
        'source_signal': 'OPEN; separate decision if confirmed product defect',
        'runtime_guard': 'UNCHANGED',
    },
}
POPULATION_ADDITIONS = (
    ('crates/ratatui-rmux/tests/ratatui_host_compat.rs', '0230350b80e65458244196fc4b46c185e84f94ab', {'tests': 68}, 68),
    ('crates/rmux-client/src/auto_start/startup_owner.rs', '6bc7505d88a62de3537f10a473293f0e9eb4b6a5', {'production': 45}, 45),
    ('crates/rmux-core/src/formats/tests/precision_c_space.rs', '399df99bff81c65309b8b665444937398cb18381', {'tests': 139}, 139),
    ('crates/rmux-core/src/grid/cell/recovery.rs', '963073e9d60eb8a0d06d2170dbfeff786157a977', {'production': 25}, 25),
    ('crates/rmux-core/src/grid/cell/reflow.rs', 'fd80d3b6db3aafc1f2900a3f14637098ddc7a944', {'production': 96}, 96),
    ('crates/rmux-core/src/input/program_status_osc.rs', 'c570d96542dd2aba8afd18823446da35aee0f50c', {'production': 27}, 27),
    ('crates/rmux-core/src/input/state_machine.rs', 'a78432bb8222d684f2d45514ffa347310a0af549', {'production': 195}, 195),
    ('crates/rmux-core/src/input/tests/dcs_opaque.rs', '7667b9a9dc2c0de16a3ef4b5e91db85f535db8a1', {'tests': 40}, 40),
    ('crates/rmux-core/src/input/tests/dcs_opaque_neighbors.rs', 'a78432bb8222d684f2d45514ffa347310a0af549', {'tests': 68}, 68),
    ('crates/rmux-core/src/keys/menu_display_tests.rs', 'e348038de8ac5cc7e71600f4a4fbca91840fc30b', {'tests': 180}, 180),
    ('crates/rmux-core/src/options/tests/window_unset_related.rs', 'fcf9544ebd679e30869b2b71f9034ddd7fcc05ae', {'tests': 150}, 150),
    ('crates/rmux-core/src/program_status.rs', '011f76dece44b415a366ca4795af341b3c17e6c0', {'production': 234, 'tests': 170}, 404),
    ('crates/rmux-core/src/recovery_row.rs', '963073e9d60eb8a0d06d2170dbfeff786157a977', {'production': 52}, 52),
    ('crates/rmux-core/src/screen/reflow_extent_tests.rs', '300e7c35f67ed6bd61289f295b01628dd74221f3', {'tests': 71}, 71),
    ('crates/rmux-core/src/screen/wrap.rs', 'c327d269bf72cc4587d18a21ab40754f40c262dc', {'production': 14}, 14),
    ('crates/rmux-ipc/src/endpoint/tests.rs', '554a650a9d0bc9345e026bca55b2298ced482403', {'tests': 508}, 508),
    ('crates/rmux-ipc/src/endpoint/tmux_compatibility.rs', '554a650a9d0bc9345e026bca55b2298ced482403', {'production': 64}, 64),
    ('crates/rmux-os/src/daemon/fd_tests.rs', 'f40d6c8067956a22d6f7cbee69e288fb3c96b8f4', {'tests': 208}, 208),
    ('crates/rmux-os/src/daemon/inherited_fds.rs', '5154fcfaae4d84e9aa9761840b211cfaeefee561', {'production': 145}, 145),
    ('crates/rmux-os/src/daemon/linux/fd_tests.rs', '6a3bc7e53b692039d45f6cef569c392185708d51', {'tests': 67}, 67),
    ('crates/rmux-os/src/daemon/startup_owner.rs', '6bc7505d88a62de3537f10a473293f0e9eb4b6a5', {'production': 26}, 26),
    ('crates/rmux-os/src/daemon/windows_launch.rs', 'cccc611a4f03e299187769e00695168f0d7a3c30', {'production': 199, 'tests': 4}, 203),
    ('crates/rmux-os/src/daemon/windows_launch/executable.rs', 'cccc611a4f03e299187769e00695168f0d7a3c30', {'production': 72}, 72),
    ('crates/rmux-os/src/daemon/windows_launch/handle_tests.rs', 'cccc611a4f03e299187769e00695168f0d7a3c30', {'tests': 85}, 85),
    ('crates/rmux-os/src/daemon/windows_launch/tests.rs', 'cccc611a4f03e299187769e00695168f0d7a3c30', {'tests': 282}, 282),
    ('crates/rmux-os/src/daemon/windows_ready.rs', 'cccc611a4f03e299187769e00695168f0d7a3c30', {'production': 173, 'tests': 50}, 223),
    ('crates/rmux-os/src/process_tree_test_support.rs', 'fe19c5772a95ad3a2e425f0d1e45a1b8a9fd4919', {'production': 80}, 80),
    ('crates/rmux-os/src/signals_tests.rs', 'ae8eb1c02f904a3925593f18d2781e4ca1780e90', {'tests': 137}, 137),
    ('crates/rmux-os/tests/socket_path_neighbors.rs', '9ec3d15e4259b542c431b14a0296e62dce5dabd6', {'tests': 41}, 41),
    ('crates/rmux-os/tests/socket_path_parent.rs', 'eac848a27e94aa4bb286322b78577a3121c5c266', {'tests': 46}, 46),
    ('crates/rmux-proto/src/program_status.rs', '011f76dece44b415a366ca4795af341b3c17e6c0', {'production': 60}, 60),
    ('crates/rmux-pty/src/backend/windows/job.rs', 'd1dcb7bc7fd25d8d44c188ab4a5834459e5bd511', {'production': 110, 'tests': 65}, 175),
    ('crates/rmux-pty/src/process_shell.rs', 'b0a8ed51c46c91d8e565ae0c4191af31d14951a8', {'production': 276}, 276),
    ('crates/rmux-pty/src/shell.rs', 'b0a8ed51c46c91d8e565ae0c4191af31d14951a8', {'production': 75, 'tests': 3}, 78),
    ('crates/rmux-pty/src/shell_tests.rs', 'b0a8ed51c46c91d8e565ae0c4191af31d14951a8', {'tests': 168}, 168),
    ('crates/rmux-pty/src/test_support/unix.rs', 'fc2d872dcb11ccf325797bd32f80aafb47edd723', {'production': 59}, 59),
    ('crates/rmux-pty/src/test_support/windows.rs', 'fc2d872dcb11ccf325797bd32f80aafb47edd723', {'production': 30}, 30),
    ('crates/rmux-sdk/examples/startup_owned_fixture.rs', 'a2f0d35e7d23facb7cf14e74e46b31a8d2596c39', {'production': 21}, 21),
    ('crates/rmux-sdk/src/handles/pane/info/parse_tests.rs', 'c0ff3b2f01afd8eb869c2fed9f04b9f20c0bbbb2', {'tests': 33}, 33),
    ('crates/rmux-sdk/src/handles/rmux/connect/startup_child.rs', '91cc5b9720c8b96eae44d98e77677882848fb2a8', {'production': 38, 'tests': 22}, 60),
    ('crates/rmux-sdk/tests/startup_owned.rs', 'a2f0d35e7d23facb7cf14e74e46b31a8d2596c39', {'tests': 138}, 138),
    ('crates/rmux-server/src/control/input_windows_tests.rs', '4a7918e0b3c722256e31991597f11a9a219e6935', {'tests': 154}, 154),
    ('crates/rmux-server/src/control/output_queue/slow_reader_tests.rs', '4a7918e0b3c722256e31991597f11a9a219e6935', {'tests': 64}, 64),
    ('crates/rmux-server/src/control/pane_event_delivery/tests/completed_block_age_tests.rs', '95f0aa88b1e7a5bed295d8115731c96b789d19a7', {'tests': 180}, 180),
    ('crates/rmux-server/src/format_runtime/tests.rs', '3b0bae37aa011eab6eb277ee687e02b150726e07', {'tests': 124}, 124),
    ('crates/rmux-server/src/format_runtime/window_name.rs', '767b71d92ed10de8a45dafd39a9c44a25eb8ca47', {'production': 151}, 151),
    ('crates/rmux-server/src/handler/attach_support/status_refresh.rs', 'fc2d872dcb11ccf325797bd32f80aafb47edd723', {'production': 81}, 81),
    ('crates/rmux-server/src/handler/client_support/listing.rs', 'fc2d872dcb11ccf325797bd32f80aafb47edd723', {'production': 92}, 92),
    ('crates/rmux-server/src/handler/client_support/switching/switch_commit.rs', 'fc2d872dcb11ccf325797bd32f80aafb47edd723', {'production': 153}, 153),
    ('crates/rmux-server/src/handler/control_support/session_update.rs', 'fc2d872dcb11ccf325797bd32f80aafb47edd723', {'production': 296}, 296),
    ('crates/rmux-server/src/handler/copy_mode_support/pipe_command_linux_tests.rs', 'fc2d872dcb11ccf325797bd32f80aafb47edd723', {'tests': 107}, 107),
    ('crates/rmux-server/src/handler/overlay_support/menu_outcome.rs', 'fc2d872dcb11ccf325797bd32f80aafb47edd723', {'production': 156}, 156),
    ('crates/rmux-server/src/handler/overlay_support/popup_job/reader.rs', 'fc2d872dcb11ccf325797bd32f80aafb47edd723', {'production': 106}, 106),
    ('crates/rmux-server/src/handler/pane_support/pane_display_panes/target.rs', 'fc2d872dcb11ccf325797bd32f80aafb47edd723', {'production': 116}, 116),
    ('crates/rmux-server/src/handler/scripting_support/targets/historical.rs', 'fc2d872dcb11ccf325797bd32f80aafb47edd723', {'production': 160}, 160),
    ('crates/rmux-server/src/handler/session_support/unattached.rs', 'fc2d872dcb11ccf325797bd32f80aafb47edd723', {'production': 121}, 121),
    ('crates/rmux-server/src/handler_alert_tests/terminal_capabilities.rs', 'b9f8e7178627379f4ab134c785ae2d4dce71dfe4', {'tests': 139}, 139),
    ('crates/rmux-server/src/handler_attach_tests/display_panes_session.rs', '53c999fa1a29ed8bd8acd67bfa1f64cc6ce61153', {'tests': 89}, 89),
    ('crates/rmux-server/src/handler_attach_tests/display_panes_session_action.rs', '780381ee804473a1cc4a815f9a96c7e0307944a4', {'tests': 48}, 48),
    ('crates/rmux-server/src/handler_attach_tests/natural_window_option.rs', 'a2ffa77182f3e8d81e48b6df692ce0bb25095c85', {'tests': 65}, 65),
    ('crates/rmux-server/src/handler_client/control_alias_occurrence_tests.rs', '42b4183b66b1ca1a3cbadbc65bcaa0b1fa563865', {'tests': 277}, 277),
    ('crates/rmux-server/src/handler_client/control_geometry_ignore_size_tests.rs', '9539d9c1aae0972a1e2f1d57906cfc3221679981', {'tests': 173}, 173),
    ('crates/rmux-server/src/handler_control_lifecycle_tests/switch_cleanup.rs', '13657c644b3b540574715387234f107b6e55db1e', {'tests': 76}, 76),
    ('crates/rmux-server/src/handler_display_message_tests/automatic_window_name.rs', '767b71d92ed10de8a45dafd39a9c44a25eb8ca47', {'tests': 155}, 155),
    ('crates/rmux-server/src/handler_display_message_tests/wrap1/conpty_attest.rs', 'e5b84dc28a7d2f9689044456217f448f697fa69c', {'tests': 347}, 347),
    ('crates/rmux-server/src/handler_lifecycle/retained_target_tests/special_paths/historical_pane/pane_identity.rs', '8ce1bf0e076da58afebb7ce9f7f0cbec24416718', {'tests': 115}, 115),
    ('crates/rmux-server/src/handler_lifecycle/retained_target_tests/special_paths/historical_pane/tombstone.rs', '3a6b40a96237d35f9151b3301ba52ee4c3a34e0f', {'tests': 275}, 275),
    ('crates/rmux-server/src/handler_mode_tree/tests/hash_render.rs', 'cda09f19c28632032aa4228aeb4241291a100bd0', {'tests': 83}, 83),
    ('crates/rmux-server/src/handler_overlay_tests/menu_execute_blocked.rs', 'cdf149e4c26ea7c8a263b9898fc6caee58a3b7af', {'tests': 187}, 187),
    ('crates/rmux-server/src/handler_overlay_tests/menu_repaint.rs', '90992ca62284aa769f47e45560cd553a4a515af7', {'tests': 99}, 99),
    ('crates/rmux-server/src/handler_overlay_tests/popup_status.rs', '627c2bf9b78f07289b5adaa07e9d739f4f9c245d', {'tests': 121}, 121),
    ('crates/rmux-server/src/handler_pane_alias_lifecycle_tests/indexed_unset.rs', 'fcf9544ebd679e30869b2b71f9034ddd7fcc05ae', {'tests': 117}, 117),
    ('crates/rmux-server/src/handler_pane_shell_tests.rs', 'd2883985811921d7b852ef651a9291cbb896e204', {'tests': 180}, 180),
    ('crates/rmux-server/src/handler_pane_state_race_tests/stale_exit_diagnostic.rs', '9074873aefcbdc4f0d40be48ddf56b975f084bbe', {'tests': 228}, 228),
    ('crates/rmux-server/src/handler_scripting/display_panes_session_tests.rs', '780381ee804473a1cc4a815f9a96c7e0307944a4', {'tests': 59}, 59),
    ('crates/rmux-server/src/handler_scripting_tests/fnp1_print_clients.rs', 'cfcb84e77fbc41cb4e766f915ef24171be956d8c', {'tests': 234}, 234),
    ('crates/rmux-server/src/handler_scripting_tests/fnp1_print_intersession.rs', 'a95c07c0edd1dd85fdf195644b52da1173d1a6d3', {'tests': 205}, 205),
    ('crates/rmux-server/src/handler_scripting_tests/natural_window_option.rs', '80fa805b659be24cd70c075905a0d1284fcc4cec', {'tests': 175}, 175),
    ('crates/rmux-server/src/handler_scripting_tests/queue_exact_target_send_keys_reset_child.rs', '580cee651833dd0f6b6ec29fb01dbc410f05d3cb', {'tests': 81}, 81),
    ('crates/rmux-server/src/handler_show_tests/hook_array_inheritance.rs', '0d5e57ab78f204af9d761ae6e0247df620199309', {'tests': 313}, 313),
    ('crates/rmux-server/src/handler_tests/list_clients_session_group.rs', '5cccaeeb76db17ef08e90bea1903b4df23c8f692', {'tests': 185}, 185),
    ('crates/rmux-server/src/handler_web_scene_test_support.rs', '17c17b4d02f29f0bc8862a06beef71125ce008d2', {'tests': 72}, 72),
    ('crates/rmux-server/src/handler_web_slow_reader_tests.rs', '93c0bc2c5202957bb03ef4990a799a088c37a576', {'tests': 69}, 69),
    ('crates/rmux-server/src/handler_wgi1_tests/removal.rs', 'f76f31782beef73a15592305487a08f2be05d61a', {'tests': 137}, 137),
    ('crates/rmux-server/src/handler_window_tests/pane_options_renumber.rs', '956d0eada5c4e3c6401950cfb506f2e88a18aab6', {'tests': 188}, 188),
    ('crates/rmux-server/src/handler_window_tests/pane_options_renumber/neighbors.rs', '38f5da84cf54940a5bc3a773fdeaac416b9d4dcd', {'tests': 333}, 333),
    ('crates/rmux-server/src/handler_window_tests/respawn_dead.rs', 'f21964c9299f33e989643a7f6de29b1f52fe734a', {'tests': 150}, 150),
    ('crates/rmux-server/src/handler_window_tests/respawn_title.rs', '8fbc0ab9ed674696e2b960d54a60144e95d5f5a8', {'tests': 98}, 98),
    ('crates/rmux-server/src/listener/startup_owner.rs', '6bc7505d88a62de3537f10a473293f0e9eb4b6a5', {'production': 38, 'tests': 2}, 40),
    ('crates/rmux-server/src/listener/startup_owner/shutdown_idle_policy_tests.rs', '2004befd0b6d329896985eb20b90c56570ec3491', {'tests': 103}, 103),
    ('crates/rmux-server/src/listener_slow_reader_tests.rs', '4a7918e0b3c722256e31991597f11a9a219e6935', {'tests': 304}, 304),
    ('crates/rmux-server/src/outer_terminal/context.rs', '7ea25f29e981f81993c3f097a561d59accbc3a6a', {'production': 89, 'tests': 8}, 97),
    ('crates/rmux-server/src/outer_terminal/report.rs', '7ea25f29e981f81993c3f097a561d59accbc3a6a', {'production': 62}, 62),
    ('crates/rmux-server/src/outer_terminal/tests/family_tests.rs', '3f64e4c97d3eae40010596736160414cc3290a08', {'tests': 109}, 109),
    ('crates/rmux-server/src/pane_io/control/pending_attach_input.rs', 'fc2d872dcb11ccf325797bd32f80aafb47edd723', {'production': 46}, 46),
    ('crates/rmux-server/src/pane_io/live_render/capture_provenance_tests.rs', '171933931bd91298cd6a5b3a534bcadc7a6c39c1', {'tests': 112}, 112),
    ('crates/rmux-server/src/pane_io/live_render/tests.rs', '171933931bd91298cd6a5b3a534bcadc7a6c39c1', {'tests': 427}, 427),
    ('crates/rmux-server/src/pane_io/persistent_overlay/popup_delta.rs', 'fc2d872dcb11ccf325797bd32f80aafb47edd723', {'production': 102, 'tests': 4}, 106),
    ('crates/rmux-server/src/pane_io/popup_delta_tests.rs', 'f8ea2b0082548dbcbab8ca0cf51b6888df550e42', {'tests': 105}, 105),
    ('crates/rmux-server/src/pane_io/reader/cursor_default_tests.rs', 'a9d96a81143757ca9fea693f9490c87ece009533', {'tests': 144}, 144),
    ('crates/rmux-server/src/pane_io/reader/program_status_tests.rs', '011f76dece44b415a366ca4795af341b3c17e6c0', {'tests': 100}, 100),
    ('crates/rmux-server/src/pane_io/tests/popup_cursor_delta.rs', '09d218f0b1c081df84652763b98b9cc123d4a359', {'tests': 224}, 224),
    ('crates/rmux-server/src/pane_io/types/overlay_frame.rs', 'fd8b6529af5b121378f954952358a69eac5d7c64', {'production': 88}, 88),
    ('crates/rmux-server/src/pane_recovery/extent_tests.rs', '71291ed52d4a6b28a9d6dba9cc5ce408ebc59fd8', {'tests': 48}, 48),
    ('crates/rmux-server/src/pane_recovery/rows.rs', '963073e9d60eb8a0d06d2170dbfeff786157a977', {'production': 125}, 125),
    ('crates/rmux-server/src/pane_terminal_process/shutdown.rs', 'e5b84dc28a7d2f9689044456217f448f697fa69c', {'production': 89, 'tests': 3}, 92),
    ('crates/rmux-server/src/pane_terminal_process/shutdown_tests.rs', 'e5b84dc28a7d2f9689044456217f448f697fa69c', {'tests': 63}, 63),
    ('crates/rmux-server/src/pane_terminals/pane_lifecycle/kill.rs', '064cb400b238ad30fb853664c3887d5dd75cb441', {'production': 351}, 351),
    ('crates/rmux-server/src/pane_terminals/pane_lifecycle/respawn.rs', '064cb400b238ad30fb853664c3887d5dd75cb441', {'production': 227}, 227),
    ('crates/rmux-server/src/pane_terminals/pane_lifecycle/window_terminal.rs', '064cb400b238ad30fb853664c3887d5dd75cb441', {'production': 163}, 163),
    ('crates/rmux-server/src/pane_terminals/pane_outputs/exit_status.rs', '9e56e69b730152293011e983c8335f7c6c87bf07', {'production': 188, 'tests': 94}, 282),
    ('crates/rmux-server/src/pane_terminals/pane_program_status.rs', '3f64e4c97d3eae40010596736160414cc3290a08', {'production': 42}, 42),
    ('crates/rmux-server/src/pane_terminals/pane_terminal_store/shutdown.rs', 'fc2d872dcb11ccf325797bd32f80aafb47edd723', {'production': 11}, 11),
    ('crates/rmux-server/src/pane_terminals/terminal_shutdown.rs', 'e5b84dc28a7d2f9689044456217f448f697fa69c', {'production': 42}, 42),
    ('crates/rmux-server/src/pane_terminals/window_support/window_respawn.rs', 'fc2d872dcb11ccf325797bd32f80aafb47edd723', {'production': 227}, 227),
    ('crates/rmux-server/src/pane_transcript/dcs_passthrough_tests.rs', 'c34326b8f37bedf48bc8d92a9b3accc47202b1ae', {'tests': 33}, 33),
    ('crates/rmux-server/src/pane_transcript/dead_footer.rs', 'c327d269bf72cc4587d18a21ab40754f40c262dc', {'production': 12}, 12),
    ('crates/rmux-server/src/pane_transcript/presentation.rs', '171933931bd91298cd6a5b3a534bcadc7a6c39c1', {'production': 273, 'tests': 4}, 277),
    ('crates/rmux-server/src/pane_transcript/presentation_test_clock.rs', '171933931bd91298cd6a5b3a534bcadc7a6c39c1', {'tests': 48}, 48),
    ('crates/rmux-server/src/renderer/overlay/popup.rs', 'f8ea2b0082548dbcbab8ca0cf51b6888df550e42', {'production': 87}, 87),
    ('crates/rmux-server/src/renderer/pane_delta/presentation_capture.rs', '171933931bd91298cd6a5b3a534bcadc7a6c39c1', {'production': 119, 'tests': 16}, 135),
    ('crates/rmux-server/src/renderer/tests/status_format_sparse.rs', '9c1fb9561160b425e41437d55f19b61a89b403eb', {'tests': 146}, 146),
    ('crates/rmux-server/src/terminal/post_exec_tests.rs', 'fb75b3ed110af9a7d612bf5faab58f0cbc0dc416', {'tests': 206}, 206),
    ('crates/rmux-server/src/terminal/profile.rs', '070897c44f76a1e6c39ecc3a1536d0225aac32ea', {'production': 509, 'tests': 44}, 553),
    ('crates/rmux-server/src/terminal/tests/program_path_spaces.rs', 'f900634f0ad06f123e85f034eb7463a339ad364f', {'tests': 152}, 152),
    ('crates/rmux-server/src/web/outbound/slow_reader_tests.rs', '93c0bc2c5202957bb03ef4990a799a088c37a576', {'tests': 110}, 110),
    ('crates/rmux-server/src/web/server_slow_reader_tests.rs', '4a7918e0b3c722256e31991597f11a9a219e6935', {'tests': 129}, 129),
    ('crates/rmux-server/tests/diagnose_direct_budget_neighbors.rs', '6268932dbdbd3e6434539d86e109fe439b32a397', {'tests': 68}, 68),
    ('crates/rmux-server/tests/injected_pane_shell.rs', 'a8a071342f23e100da9697c50fbf97f2c0133539', {'tests': 331}, 331),
    ('crates/rmux-server/tests/queue_inventory.rs', '9499d76f65c9250907a44215c38bb155affb3d59', {'tests': 86}, 86),
    ('crates/rmux-server/tests/signal_watcher_fd_lifecycle.rs', '049e5fa403c78b2bbcdb50da6d80e5cd6c054641', {'tests': 33}, 33),
    ('crates/rmux-server/tests/windows_cfg_build.rs', '71845df5818c57f6dfb940117ead99376a7c581a', {'tests': 12}, 12),
    ('scripts/rust-regex-validator/src/main.rs', '4cb3111829613c559b8d2d7a7f742989961e3e53', {'production': 18}, 18),
    ('src/cli/dispatch/display_panes_canonical_tests.rs', 'ca00a0a8c374205801b25c3c2304ec51c44a9609', {'tests': 91}, 91),
    ('src/cli/dispatch/display_panes_rows_tests.rs', '584b0093077aa84684c9247d05fd28af37b05bf4', {'tests': 94}, 94),
    ('src/cli/dispatch/display_panes_tests.rs', '780381ee804473a1cc4a815f9a96c7e0307944a4', {'tests': 106}, 106),
    ('src/cli/display_panes.rs', '6347b4415b94ecb7b1deccec999d311c687ecb37', {'production': 73}, 73),
    ('src/cli/session_commands/tests.rs', '4a60c2f5d8f049255a86a3afb6141caca7f14e73', {'tests': 193}, 193),
    ('src/cli/tests.rs', '04e6e88148c1dfa4d0b6ee6ad92b58ef9bf104b2', {'tests': 405}, 405),
    ('src/cli_args/pane/required_dimensions.rs', 'e68a6327b793e4c5adb2501a80caceb87a7df8e4', {'production': 34}, 34),
    ('src/cli_args/pane/required_dimensions_tests.rs', '7d3faf271adb382e439b89acda9d76b8698e0e35', {'tests': 45}, 45),
    ('src/cli_args_tests/option_value_ownership.rs', '7f8ccc9a659841898f9b6957571778c71ca6ff4a', {'tests': 139}, 139),
    ('src/cli_args_tests/resize_value_boundaries.rs', '586ef58faaaf95444faaa555277200776a966df3', {'tests': 82}, 82),
    ('src/cli_args_tests/resize_value_ownership.rs', '7d3faf271adb382e439b89acda9d76b8698e0e35', {'tests': 60}, 60),
    ('src/session_inventory_tests.rs', 'c0ff3b2f01afd8eb869c2fed9f04b9f20c0bbbb2', {'tests': 166}, 166),
    ('src/tiny_main/tests/commands.rs', '64f702c00201c3abed14ceda85ba87c5745c739a', {'tests': 261}, 261),
    ('src/tiny_main/tests/fuzz.rs', '64f702c00201c3abed14ceda85ba87c5745c739a', {'tests': 109}, 109),
    ('src/tiny_main/tests/responses.rs', '64f702c00201c3abed14ceda85ba87c5745c739a', {'tests': 176}, 176),
    ('src/tiny_main/tests/routing.rs', '64f702c00201c3abed14ceda85ba87c5745c739a', {'tests': 288}, 288),
    ('src/tiny_main/tests/targets.rs', '64f702c00201c3abed14ceda85ba87c5745c739a', {'tests': 354}, 354),
    ('src/tiny_main/tests/windows.rs', '64f702c00201c3abed14ceda85ba87c5745c739a', {'tests': 96}, 96),
    ('tests/cli_display_panes_session.rs', '780381ee804473a1cc4a815f9a96c7e0307944a4', {'tests': 43}, 43),
    ('tests/cli_error_contract.rs', 'd1dcb7bc7fd25d8d44c188ab4a5834459e5bd511', {'tests': 266}, 266),
    ('tests/cli_error_contract/invalid_flag.rs', '630f5e05d87238dfe17ede99e211faf486e9401a', {'tests': 54}, 54),
    ('tests/cli_long_help_windows.rs', 'f3992e7867e51d90f7bebef07843bb2737e60c7e', {'tests': 148}, 148),
    ('tests/cli_long_version_windows.rs', 'bf464d0567aac5dfc58e13d71bfb3a502a7e5cff', {'tests': 135}, 135),
    ('tests/cli_new_session_client_size.rs', 'f9fa5f57b0cedc1109d0669f304db9e92d96b3f1', {'tests': 88}, 88),
    ('tests/daemon_shutdown_barrier.rs', 'e5b84dc28a7d2f9689044456217f448f697fa69c', {'tests': 126}, 126),
    ('tests/diagnose_compatibility.rs', 'cb65bb012482367a3cadfd981bad335a299b95e5', {'tests': 76}, 76),
    ('tests/popup_burst_attach.rs', '0bcd24978b155abe18c2d36db012b70a014a94b5', {'tests': 184}, 184),
    ('tests/popup_burst_attach_windows.rs', '094fc8990380416f9353c0087330ba0638cc262a', {'tests': 326}, 326),
    ('tests/popup_repaint.rs', '44d5efc40717a9a2f29b74d421f5a34291d89e94', {'tests': 231}, 231),
    ('tests/queued_server_startup.rs', '3fe2bb2a7e7a2132b965322c107043543d96acd5', {'tests': 26}, 26),
    ('tests/startup_config_acceptance.rs', 'e5b84dc28a7d2f9689044456217f448f697fa69c', {'tests': 85}, 85),
    ('tests/support/daemon_fd_inheritance.rs', '2714e4c2cb9f28bc27b528a8d4e8b86ba2d295b8', {'tests': 91}, 91),
    ('tests/support/startup_stderr.rs', 'c5b418b878d3d42a5ee85458f30652880c30a8cf', {'tests': 131}, 131),
    ('tests/support/windows_job_bound.rs', 'e72b09c56d3c520351deec84deb00acc83978d2b', {'tests': 67}, 67),
    ('tests/tmux_compat_harness/owned_rescue.rs', '496663983eebee4e56e51990e16484c3278f6d9c', {'tests': 261}, 261),
    ('tests/tmux_compat_surface_matrix/late_prompt.rs', 'eb9a932636e3cbb36c1174cbcb5605285b33493b', {'tests': 59}, 59),
    ('tests/tmux_compat_surface_matrix/shell_absent.rs', '02fc79bdcf69156d448c42308c45737617d02cd1', {'tests': 90}, 90),
    ('tests/windows_job_bound_startup.rs', '6cde004e67fac30bd25637846d8f979b648ea026', {'tests': 143}, 143),
    ('tests/windows_new_session_client_size.rs', 'f9fa5f57b0cedc1109d0669f304db9e92d96b3f1', {'tests': 217}, 217),
    ('xtask/src/registry_docs/manual_context.rs', '88be9c5a57f945ffc40d891e73ce8715d3f0af18', {'tooling': 365}, 365),
    ('xtask/src/registry_docs/tests/references.rs', 'a57d4cce8996b88a4dd195e5942a06d7b10aa41c', {'tests': 276}, 276),
)


# D576(1): only the 17 additional Rust paths in the composed 2d77b122 tree.
# Each tuple binds to its contributing parent; the D573 epoch stays immutable.
# Counts are from the exact composed scan. Semantic slots remain synthetic,
# and SOURCE-SEMANTICS stays OPEN under D573(3), with the real guard unchanged.
COMPOSED_POPULATION_EPOCH = {
    'epoch': 'D576-2d77b122-population',
    'revision': '2d77b122d1f376b9f49674d77e5293eb6ba6f5a3',
    'parents': ('988eac4e8ba7633e13fab69d090cbfccca135a28',
                '069507857402a6a72d1a38a8720d3ca954a2c21e'),
    'decision': 'D576(1)-(3), 2026-11-03T15:48:47Z',
    'historical_population': 2486,
    'population': 2503,
}
COMPOSED_POPULATION_ADDITIONS = (
    ('crates/rmux-core/src/options/tests/pane_override_self_swap.rs', '988eac4e8ba7633e13fab69d090cbfccca135a28', {'tests': 118}, 118),
    ('crates/rmux-server/src/handler_mode_tree/tests/buffer_editor_current.rs', '988eac4e8ba7633e13fab69d090cbfccca135a28', {'tests': 339}, 339),
    ('crates/rmux-server/src/handler_prompt/history_file/startup_shutdown_tests.rs', '988eac4e8ba7633e13fab69d090cbfccca135a28', {'tests': 166}, 166),
    ('crates/rmux-server/src/handler_scripting_tests/run_shell/deferred_attach.rs', '988eac4e8ba7633e13fab69d090cbfccca135a28', {'tests': 156}, 156),
    ('crates/rmux-server/src/handler_scripting_tests/run_shell/deferred_attach_siblings.rs', '988eac4e8ba7633e13fab69d090cbfccca135a28', {'tests': 44}, 44),
    ('crates/rmux-server/src/pane_io/reader/publication.rs', '988eac4e8ba7633e13fab69d090cbfccca135a28', {'production': 351, 'tests': 119}, 470),
    ('crates/rmux-server/src/pane_io/reader/rename_admission_native_tests.rs', '988eac4e8ba7633e13fab69d090cbfccca135a28', {'tests': 373}, 373),
    ('crates/rmux-server/src/pane_io/reader/rename_admission_tests.rs', '988eac4e8ba7633e13fab69d090cbfccca135a28', {'tests': 190}, 190),
    ('crates/rmux-server/src/pane_io/reader/tests.rs', '988eac4e8ba7633e13fab69d090cbfccca135a28', {'tests': 531}, 531),
    ('crates/rmux-server/src/pane_io/reader/timers.rs', '988eac4e8ba7633e13fab69d090cbfccca135a28', {'production': 206}, 206),
    ('crates/rmux-server/src/pane_io/reader/unix_tests.rs', '988eac4e8ba7633e13fab69d090cbfccca135a28', {'tests': 358}, 358),
    ('crates/rmux-server/src/pane_io/reader/windows.rs', '988eac4e8ba7633e13fab69d090cbfccca135a28', {'production': 49, 'tests': 2}, 51),
    ('crates/rmux-server/src/pane_io/reader/windows_reply_probe.rs', '988eac4e8ba7633e13fab69d090cbfccca135a28', {'tests': 62}, 62),
    ('crates/rmux-server/src/pane_io/reader/windows_tests.rs', '988eac4e8ba7633e13fab69d090cbfccca135a28', {'tests': 153}, 153),
    ('crates/rmux-server/src/pane_io/types/publication.rs', '988eac4e8ba7633e13fab69d090cbfccca135a28', {'production': 70, 'tests': 47}, 117),
    ('crates/rmux-server/src/pane_terminals/input_capture_tests_support.rs', '988eac4e8ba7633e13fab69d090cbfccca135a28', {'tests': 119}, 119),
    ('crates/rmux-server/src/renderer/scrollbar_only_tests.rs', '988eac4e8ba7633e13fab69d090cbfccca135a28', {'tests': 204}, 204),
)


# D583(1): only the five lot13 Rust additions on 08fa942b plus S1220.
# Prior epochs and synthetic semantic slots stay unchanged; D573(3) stays OPEN.
LOT13_POPULATION_EPOCH = {
    'epoch': 'D583-d8e25647-population',
    'revision': 'd8e256476b998b1b9b7aebc5a8f7c890421fa430',
    'decision': 'D583(1)-(3), 2026-11-03T17:53:51Z',
    'historical_population': 2503,
    'population': 2508,
}
LOT13_POPULATION_ADDITIONS = (
    ('crates/rmux-core/src/screen/capture/physical.rs', '1b75b714c8587bb7fede437fc799e4671397899f', {'production': 88}, 88),
    ('crates/rmux-server/src/handler_capture_tests/capture_format.rs', '00d621482f45dcdc8f76ad01fda8057175be58bf', {'tests': 152}, 152),
    ('crates/rmux-server/src/handler_capture_tests/capture_format_empty_print.rs', 'ec4591e2d0bacd9519c8000b29eadb9b2e2d8833', {'tests': 35}, 35),
    ('crates/rmux-server/src/handler_capture_tests/capture_format_neighbors.rs', 'e86fbc33db02a0f638595fd99ccb361d9877006d', {'tests': 510}, 510),
    ('crates/rmux-server/src/pane_terminals/pane_transcripts/capture.rs', 'e86fbc33db02a0f638595fd99ccb361d9877006d', {'production': 157}, 157),
)


# Population measured at c33a27a8: the 22 paths added after the lot13 fixture.
# Counts come from the exact-tree scan with inline module resolution corrected;
# framing.rs is test-only. This is synthetic population coverage, not a new
# semantic admission: historical rows, proof epochs and the real-scan guard stay.
CURRENT_POPULATION_EPOCH = {
    'revision': 'c33a27a8df6f0772231fa6d5700970fab4ee6aad',
    'historical_population': 2508,
    'population': 2530,
}
CURRENT_POPULATION_ADDITIONS = (
    ('crates/rmux-client/src/attach_windows/console_mode_child_selection.rs', '3d6d9408d9c1147d75be85325861bad88b5b387f', {'tests': 110}, 110),
    ('crates/rmux-proto/src/source_file_input.rs', 'ee96a8d08c42ff378a3bdb2ab0434ff1b5ca546f', {'production': 65}, 65),
    ('crates/rmux-proto/tests/source_file_input_ledger.rs', '364ffe9f3b513ec931c23d78e6ba02c971ebf14c', {'tests': 51}, 51),
    ('crates/rmux-proto/tests/source_file_input_v1.rs', 'ee96a8d08c42ff378a3bdb2ab0434ff1b5ca546f', {'tests': 109}, 109),
    ('crates/rmux-server/src/handler/scripting_support/invocation_input.rs', 'ee96a8d08c42ff378a3bdb2ab0434ff1b5ca546f', {'production': 26}, 26),
    ('crates/rmux-server/src/handler/scripting_support/source_files/budget_tests.rs', '779ddf5e7d07ff84eb580675587489743c06445b', {'tests': 132}, 132),
    ('crates/rmux-server/src/handler/scripting_support/source_files/direct_budget_tests.rs', '304808efde25ccd495dded6137a66c521a67001a', {'tests': 100}, 100),
    ('crates/rmux-server/src/handler/scripting_support/source_input_admission.rs', 'ee96a8d08c42ff378a3bdb2ab0434ff1b5ca546f', {'production': 104, 'tests': 3}, 107),
    ('crates/rmux-server/src/handler/scripting_support/source_input_admission_tests.rs', 'ee96a8d08c42ff378a3bdb2ab0434ff1b5ca546f', {'tests': 420}, 420),
    ('crates/rmux-server/src/handler/scripting_support/source_input_diagnostic_tests.rs', '027c913df99b29732958a572ba16917599f80b6e', {'tests': 192}, 192),
    ('crates/rmux-server/src/handler/scripting_support/source_input_queue_status.rs', '81c2162af7cf0b5fcf37bab566dc9d23eacb4814', {'production': 46, 'tests': 15}, 61),
    ('crates/rmux-server/src/handler/scripting_support/source_input_tests.rs', 'eff245e990a7c3cf1c379254a1f51f3d61ea99fb', {'tests': 79}, 79),
    ('crates/rmux-server/src/handler_scripting_tests/source_input_marked_context.rs', 'fb8e2f74c4aec04065f2c53d16ddb8eee826077c', {'tests': 236}, 236),
    ('src/cli/alias_fallback/source_file_input.rs', 'ee96a8d08c42ff378a3bdb2ab0434ff1b5ca546f', {'production': 134, 'tests': 86}, 220),
    ('src/cli/json_output/tests/framing.rs', 'c4ecf3f22ab1e76217031097b88f18c941c2ff0e', {'tests': 165}, 165),
    ('src/cli_test_inventory_tests.rs', 'c4ecf3f22ab1e76217031097b88f18c941c2ff0e', {'tests': 30}, 30),
    ('tests/cli_schema_full_tiny_equivalence/scratch.rs', '39716a0da3cfec6c11e5276f7b2cfcc78bd39c5f', {'tests': 27}, 27),
    ('tests/cli_surface/source_file_queue.rs', 'dbeb5550b24c726b8a4b7b800e12581e84fcaba4', {'tests': 428}, 428),
    ('tests/common/empty_socket_lock.rs', 'b76658dc6c1f05026ab42eceb349a64d3b18b485', {'tests': 69}, 69),
    ('tests/empty_socket_lock.rs', 'ad9c897b3b271bd8fe460492ae9c0d27e8d49b7d', {'tests': 117}, 117),
    ('tests/server_invalid_flag.rs', '046d96dc2961d5fdc1958966a200e5cd3e826bed', {'tests': 94}, 94),
    ('tests/window_json_layout.rs', 'f6b223c4acf6d952bcff9825052e2f5b2352c2b7', {'tests': 198}, 198),
)


# Rust population measured at 3e9d1ecc; document freshness is not certified.
# Append only these three test sources; prior epochs and semantic guards stay.
DESCENDANT_POPULATION_EPOCH = {
    'revision': '3e9d1ecc10f9cbf0ac569fdd1dcd3417063182cc',
    'historical_population': 2530,
    'population': 2533,
}
DESCENDANT_POPULATION_ADDITIONS = (
    ('tests/scripting/attach_targets.rs', '8019e47ff85982cbaab6dcb5454fd25521c25974', {'tests': 85}, 85),
    ('tests/scripting/has_targets.rs', '467b16f3ff75c848604de1a24b4758c7c409237a', {'tests': 252}, 252),
    ('tests/support/bench_python.rs', '5c6449be59dcf38ce037f89b142383b4615db55a', {'tests': 110}, 110),
)


# Rust population measured at 4f8fa51c: the fourteen child modules of the S1764
# size split, counted by the real scan. Synthetic population coverage only;
# prior epochs, size caps and the real-scan semantic guard stay unchanged.
SIZE_SPLIT_POPULATION_EPOCH = {
    'revision': '4f8fa51cc20bfce2429b507a3d9b1554dfdec837',
    'historical_population': 2533,
    'population': 2547,
}
SIZE_SPLIT_POPULATION_ADDITIONS = (
    ('crates/rmux-core/src/screen/metadata.rs', '4f8fa51cc20bfce2429b507a3d9b1554dfdec837', {'production': 74}, 74),
    ('crates/rmux-proto/src/frame_kind/vocabulary.rs', '4f8fa51cc20bfce2429b507a3d9b1554dfdec837', {'production': 115}, 115),
    ('crates/rmux-proto/src/request/shell.rs', '4f8fa51cc20bfce2429b507a3d9b1554dfdec837', {'production': 76}, 76),
    ('crates/rmux-proto/src/response/pane/snapshot.rs', '4f8fa51cc20bfce2429b507a3d9b1554dfdec837', {'production': 74}, 74),
    ('crates/rmux-proto/src/response/routing.rs', '4f8fa51cc20bfce2429b507a3d9b1554dfdec837', {'production': 196}, 196),
    ('crates/rmux-server/src/format_runtime/variables/search.rs', '4f8fa51cc20bfce2429b507a3d9b1554dfdec837', {'production': 65}, 65),
    ('crates/rmux-server/src/handler/alert_support/pane_events/window_renames.rs', '4f8fa51cc20bfce2429b507a3d9b1554dfdec837', {'production': 110}, 110),
    ('crates/rmux-server/src/handler/attach_support/switch_commit/publication.rs', '4f8fa51cc20bfce2429b507a3d9b1554dfdec837', {'production': 64}, 64),
    ('crates/rmux-server/src/handler/pane_support/pane_io_encoding/committed_focus.rs', '4f8fa51cc20bfce2429b507a3d9b1554dfdec837', {'production': 71, 'tests': 6}, 77),
    ('crates/rmux-server/src/handler/pane_support/pane_lifecycle/dead_pane_footer.rs', '4f8fa51cc20bfce2429b507a3d9b1554dfdec837', {'production': 103}, 103),
    ('crates/rmux-server/src/handler/scripting_support/config_parse/set_option_scope.rs', '4f8fa51cc20bfce2429b507a3d9b1554dfdec837', {'production': 258}, 258),
    ('crates/rmux-server/src/handler/scripting_support/source_runtime/verbose.rs', '4f8fa51cc20bfce2429b507a3d9b1554dfdec837', {'production': 168}, 168),
    ('crates/rmux-server/src/handler/scripting_support/targets/independence.rs', '4f8fa51cc20bfce2429b507a3d9b1554dfdec837', {'production': 122}, 122),
    ('crates/rmux-server/src/pane_terminals/deferred_initial/starting_input.rs', '4f8fa51cc20bfce2429b507a3d9b1554dfdec837', {'production': 105}, 105),
)


# Suite S1784: 7420efe2 added writer/metadata.rs without declaring it, so every
# descendant refused the exact-population check. One row, counted by the real
# scan; prior epochs, size caps and the real-scan semantic guard stay unchanged.
WRITER_METADATA_POPULATION_EPOCH = {
    'revision': '7420efe2b28e42962e42212b674093a6d7298b67',
    'historical_population': 2547,
    'population': 2548,
}
WRITER_METADATA_POPULATION_ADDITIONS = (
    ('crates/rmux-core/src/screen/writer/metadata.rs', '7420efe2b28e42962e42212b674093a6d7298b67', {'production': 48}, 48),
)


# Suite S1844: six integrated splits (1a517c30, 8b617039, 726e1939, 72d913b3,
# 02d20d8f, e432dee2) added these child modules without declaring them, so every
# descendant refused the exact-population check. Rows counted by the real scan at
# 9fc22d70; none carries a guarded semantic attribute. Prior epochs, size caps and
# the real-scan semantic guard stay unchanged.
MODULE_SPLIT_POPULATION_EPOCH = {
    'revision': '9fc22d7008d19744d762c94b7e214c3530db0110',
    'historical_population': 2548,
    'population': 2595,
}
MODULE_SPLIT_POPULATION_ADDITIONS = (
    ('crates/rmux-core/src/command_parser/error.rs', '726e193965bb644694b07523e81cd998e3515fa1', {'production': 80}, 80),
    ('crates/rmux-core/src/command_parser/escape.rs', '726e193965bb644694b07523e81cd998e3515fa1', {'production': 114}, 114),
    ('crates/rmux-core/src/command_parser/model.rs', '726e193965bb644694b07523e81cd998e3515fa1', {'production': 354}, 354),
    ('crates/rmux-core/src/environment/name_keys.rs', '1a517c30e04fba15cd4ee1812b7df44f8e2176f3', {'production': 108}, 108),
    ('crates/rmux-core/src/formats/path_names.rs', '726e193965bb644694b07523e81cd998e3515fa1', {'production': 81}, 81),
    ('crates/rmux-core/src/formats/resolve.rs', '726e193965bb644694b07523e81cd998e3515fa1', {'production': 74}, 74),
    ('crates/rmux-core/src/formats/value_modifiers.rs', '726e193965bb644694b07523e81cd998e3515fa1', {'production': 87}, 87),
    ('crates/rmux-core/src/grid/cell/extent.rs', '72d913b39ea5c47847a4690a2a2f6562d053dba7', {'production': 109}, 109),
    ('crates/rmux-core/src/grid/cell/grid_cell.rs', '72d913b39ea5c47847a4690a2a2f6562d053dba7', {'production': 265}, 265),
    ('crates/rmux-core/src/grid/cell/plain_text.rs', '72d913b39ea5c47847a4690a2a2f6562d053dba7', {'production': 185}, 185),
    ('crates/rmux-core/src/grid/cell/render.rs', '72d913b39ea5c47847a4690a2a2f6562d053dba7', {'production': 357}, 357),
    ('crates/rmux-core/src/keys/store/default_bind.rs', '1a517c30e04fba15cd4ee1812b7df44f8e2176f3', {'production': 88}, 88),
    ('crates/rmux-core/src/options/lifecycle.rs', '726e193965bb644694b07523e81cd998e3515fa1', {'production': 445}, 445),
    ('crates/rmux-core/src/screen/combine.rs', '72d913b39ea5c47847a4690a2a2f6562d053dba7', {'production': 168}, 168),
    ('crates/rmux-core/src/screen/cursor.rs', '72d913b39ea5c47847a4690a2a2f6562d053dba7', {'production': 57}, 57),
    ('crates/rmux-core/src/screen/erase.rs', '72d913b39ea5c47847a4690a2a2f6562d053dba7', {'production': 142}, 142),
    ('crates/rmux-core/src/screen/line_deletion.rs', '72d913b39ea5c47847a4690a2a2f6562d053dba7', {'production': 75}, 75),
    ('crates/rmux-core/src/screen/print.rs', '72d913b39ea5c47847a4690a2a2f6562d053dba7', {'production': 251}, 251),
    ('crates/rmux-core/src/screen/synchronized_output.rs', '72d913b39ea5c47847a4690a2a2f6562d053dba7', {'production': 50}, 50),
    ('crates/rmux-core/src/screen/view/recovery.rs', '1a517c30e04fba15cd4ee1812b7df44f8e2176f3', {'production': 261}, 261),
    ('crates/rmux-core/src/session/store/grouped.rs', '1a517c30e04fba15cd4ee1812b7df44f8e2176f3', {'production': 213}, 213),
    ('crates/rmux-core/src/session/window_ops/slot_moves.rs', '1a517c30e04fba15cd4ee1812b7df44f8e2176f3', {'production': 245}, 245),
    ('crates/rmux-core/src/window/floating/stack.rs', '1a517c30e04fba15cd4ee1812b7df44f8e2176f3', {'production': 174}, 174),
    ('crates/rmux-server/src/handler/alert_support/pane_events/prepare.rs', '8b617039a8dd724cf8440b226e736fa81418858f', {'production': 134, 'tests': 8}, 142),
    ('crates/rmux-server/src/handler/attach_support/transient_message/ignored_scan.rs', '8b617039a8dd724cf8440b226e736fa81418858f', {'production': 126}, 126),
    ('crates/rmux-server/src/handler/buffer_support/paste.rs', 'e432dee2d78a97edc115fda0340c7a3702ddf52e', {'production': 352, 'tests': 13}, 365),
    ('crates/rmux-server/src/handler/client_support/suspend.rs', '8b617039a8dd724cf8440b226e736fa81418858f', {'production': 58}, 58),
    ('crates/rmux-server/src/handler/config_support/environment.rs', '8b617039a8dd724cf8440b226e736fa81418858f', {'production': 187}, 187),
    ('crates/rmux-server/src/handler/option_support/scope_targets.rs', '8b617039a8dd724cf8440b226e736fa81418858f', {'production': 95}, 95),
    ('crates/rmux-server/src/handler/pane_state_support/snapshot.rs', 'e432dee2d78a97edc115fda0340c7a3702ddf52e', {'production': 161}, 161),
    ('crates/rmux-server/src/handler/pane_stream_support/raw/initialization.rs', '8b617039a8dd724cf8440b226e736fa81418858f', {'production': 184}, 184),
    ('crates/rmux-server/src/handler/pane_support/pane_by_id/target_ref.rs', '8b617039a8dd724cf8440b226e736fa81418858f', {'production': 56}, 56),
    ('crates/rmux-server/src/handler/pane_support/pane_layout/layout_options.rs', 'e432dee2d78a97edc115fda0340c7a3702ddf52e', {'production': 240}, 240),
    ('crates/rmux-server/src/handler/prompt_support/lifecycle/completion.rs', 'e432dee2d78a97edc115fda0340c7a3702ddf52e', {'production': 218}, 218),
    ('crates/rmux-server/src/handler/scripting_support/client_parse/server_access.rs', '8b617039a8dd724cf8440b226e736fa81418858f', {'production': 108}, 108),
    ('crates/rmux-server/src/handler/scripting_support/display_parse/pane_history.rs', '02d20d8f7022f500bee03d1c1696724fa25d9ca3', {'production': 150}, 150),
    ('crates/rmux-server/src/handler/scripting_support/layout_parse/resize_adjustment.rs', '02d20d8f7022f500bee03d1c1696724fa25d9ca3', {'production': 129}, 129),
    ('crates/rmux-server/src/handler/scripting_support/new_window_runtime/target_witness.rs', '8b617039a8dd724cf8440b226e736fa81418858f', {'production': 172}, 172),
    ('crates/rmux-server/src/handler/scripting_support/queue/command_action.rs', 'e432dee2d78a97edc115fda0340c7a3702ddf52e', {'production': 100}, 100),
    ('crates/rmux-server/src/handler/scripting_support/queue/session_rename.rs', 'e432dee2d78a97edc115fda0340c7a3702ddf52e', {'production': 120}, 120),
    ('crates/rmux-server/src/handler/scripting_support/window_parse/new_window.rs', 'e432dee2d78a97edc115fda0340c7a3702ddf52e', {'production': 185}, 185),
    ('crates/rmux-server/src/handler/subscription_support/exited_drain.rs', '8b617039a8dd724cf8440b226e736fa81418858f', {'production': 96}, 96),
    ('crates/rmux-server/src/handler/target_support/request_response.rs', '02d20d8f7022f500bee03d1c1696724fa25d9ca3', {'production': 190}, 190),
    ('crates/rmux-server/src/handler/web_request_identity/mutation_checks.rs', '02d20d8f7022f500bee03d1c1696724fa25d9ca3', {'production': 116}, 116),
    ('crates/rmux-server/src/pane_terminals/pane_transfer/grouped/runtime_moves.rs', '8b617039a8dd724cf8440b226e736fa81418858f', {'production': 98}, 98),
    ('crates/rmux-server/src/renderer/format_draw/clause_parse.rs', '02d20d8f7022f500bee03d1c1696724fa25d9ca3', {'production': 237}, 237),
    ('crates/rmux-server/src/renderer/projection/occlusion.rs', '02d20d8f7022f500bee03d1c1696724fa25d9ca3', {'production': 141}, 141),
)


# Suite S1849: the trunk splits 3a02c14d, 9afb4153 and 9c346e8e added these
# child modules without declaring them, so every descendant refused the
# exact-population check. Rows counted by the real scan at 642fdeaf, after
# the two status window-run allowances returned to renderer/status.rs, so none
# carries a guarded semantic attribute. Prior epochs, size caps and the real-scan
# semantic guard stay unchanged.
TRUNK_SPLIT_POPULATION_EPOCH = {
    'revision': '642fdeaf9fb5451877f7400867c4bf7f4cbc183d',
    'historical_population': 2595,
    'population': 2614,
}
TRUNK_SPLIT_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/handler/copy_mode_support/command_context.rs', '3a02c14db0dd38908981b16388ece0a1b2467db4', {'production': 164}, 164),
    ('crates/rmux-server/src/handler/copy_mode_support/transfer.rs', '3a02c14db0dd38908981b16388ece0a1b2467db4', {'production': 186}, 186),
    ('crates/rmux-server/src/handler/pane_stream_support/guards.rs', '9afb415316aa11bbd23ab9d1741ab601025d1ae9', {'production': 160}, 160),
    ('crates/rmux-server/src/handler/pane_stream_support/surface_subscription.rs', '9afb415316aa11bbd23ab9d1741ab601025d1ae9', {'production': 139}, 139),
    ('crates/rmux-server/src/handler/scripting_support/config_parse/environment.rs', '9c346e8edce2971a21e12efeb0b6dda373d7eecf', {'production': 189}, 189),
    ('crates/rmux-server/src/handler/scripting_support/config_parse/show_options.rs', '9c346e8edce2971a21e12efeb0b6dda373d7eecf', {'production': 467}, 467),
    ('crates/rmux-server/src/handler/scripting_support/pane_parse/select_pane.rs', '9c346e8edce2971a21e12efeb0b6dda373d7eecf', {'production': 183}, 183),
    ('crates/rmux-server/src/handler/scripting_support/pane_parse/split_window.rs', '9c346e8edce2971a21e12efeb0b6dda373d7eecf', {'production': 314}, 314),
    ('crates/rmux-server/src/pane_recovery/paint.rs', '9afb415316aa11bbd23ab9d1741ab601025d1ae9', {'production': 210}, 210),
    ('crates/rmux-server/src/pane_recovery/row_capture.rs', '9afb415316aa11bbd23ab9d1741ab601025d1ae9', {'production': 145}, 145),
    ('crates/rmux-server/src/pane_terminals/window_support/window_movement/cross_session/linked.rs', '9afb415316aa11bbd23ab9d1741ab601025d1ae9', {'production': 178}, 178),
    ('crates/rmux-server/src/pane_terminals/window_support/window_movement/cross_session/swap.rs', '9afb415316aa11bbd23ab9d1741ab601025d1ae9', {'production': 280}, 280),
    ('crates/rmux-server/src/renderer/pane_delta/diff.rs', '9afb415316aa11bbd23ab9d1741ab601025d1ae9', {'production': 211}, 211),
    ('crates/rmux-server/src/renderer/pane_delta/plain_output.rs', '9afb415316aa11bbd23ab9d1741ab601025d1ae9', {'production': 254, 'tests': 28}, 282),
    ('crates/rmux-server/src/renderer/status/window_list.rs', '9afb415316aa11bbd23ab9d1741ab601025d1ae9', {'production': 212}, 212),
    ('crates/rmux-server/src/web/registry/auth.rs', '9afb415316aa11bbd23ab9d1741ab601025d1ae9', {'production': 175, 'tests': 46}, 221),
    ('crates/rmux-server/src/web/registry/create.rs', '9afb415316aa11bbd23ab9d1741ab601025d1ae9', {'production': 297}, 297),
    ('crates/rmux-server/src/web/server/streams/pane_loop.rs', '3a02c14db0dd38908981b16388ece0a1b2467db4', {'production': 263}, 263),
    ('crates/rmux-server/src/web/server/streams/session_loop.rs', '3a02c14db0dd38908981b16388ece0a1b2467db4', {'production': 403, 'tests': 6}, 409),
)


# Suite S1850: 03adcd3d moved the inline tests of request_parse.rs into one child
# module. One row, counted by the real scan at 03adcd3d (2614 -> 2615 Rust files);
# no guarded semantic attribute. Prior epochs, size caps and the real-scan
# semantic guard stay unchanged.
REQUEST_PARSE_SPLIT_POPULATION_EPOCH = {
    'revision': '03adcd3d1235dbee829eacddc1f6f04c8abb22e9',
    'historical_population': 2614,
    'population': 2615,
}
REQUEST_PARSE_SPLIT_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/handler/scripting_support/request_parse/tests.rs', '03adcd3d1235dbee829eacddc1f6f04c8abb22e9', {'tests': 541}, 541),
)


# Suite S1856: 647ce449 split src/cli_args/pane.rs into two child modules
# (S1851). Two rows, counted by the real scan at 647ce449 (2615 -> 2617 Rust
# files); no guarded semantic attribute. Prior epochs, size caps and the
# real-scan semantic guard stay unchanged.
CLI_ARGS_PANE_SPLIT_POPULATION_EPOCH = {
    'revision': '647ce4492e7c87f9112e1cf568b6a1b1601d7b33',
    'historical_population': 2615,
    'population': 2617,
}
CLI_ARGS_PANE_SPLIT_POPULATION_ADDITIONS = (
    ('src/cli_args/pane/l1_034_tests.rs', '647ce4492e7c87f9112e1cf568b6a1b1601d7b33', {'tests': 64}, 64),
    ('src/cli_args/pane/resize_pane.rs', '647ce4492e7c87f9112e1cf568b6a1b1601d7b33', {'production': 448}, 448),
)


# Suite S1874: split attached pane input by modal, mouse and session responsibility.
# Three children counted by the real scan; prior epochs and guards are unchanged.
ATTACHED_INPUT_SPLIT_POPULATION_EPOCH = {
    'revision': '17ebbdc303da9ff22851cba4f97576fce55e275d',
    'historical_population': 2617,
    'population': 2620,
}
ATTACHED_INPUT_SPLIT_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/handler/pane_support/pane_attached_input/modal_input.rs', '17ebbdc303da9ff22851cba4f97576fce55e275d', {'production': 384, 'tests': 8}, 392),
    ('crates/rmux-server/src/handler/pane_support/pane_attached_input/mouse_input.rs', '17ebbdc303da9ff22851cba4f97576fce55e275d', {'production': 249, 'tests': 2}, 251),
    ('crates/rmux-server/src/handler/pane_support/pane_attached_input/session_state.rs', '17ebbdc303da9ff22851cba4f97576fce55e275d', {'production': 152, 'tests': 22}, 174),
)


# Suite S1868: four grid storage/geometry modules, counted by the real scan
# at 70454e37. Prior epochs, size caps and semantic guards stay unchanged.
GRID_SPLIT_POPULATION_EPOCH = {
    'revision': '70454e372e21c6f45d2de271bbe25cf11b188dff',
    'historical_population': 2620,
    'population': 2624,
}
GRID_SPLIT_POPULATION_ADDITIONS = (
    ('crates/rmux-core/src/grid/history.rs', '70454e372e21c6f45d2de271bbe25cf11b188dff', {'production': 117}, 117),
    ('crates/rmux-core/src/grid/reflow.rs', '70454e372e21c6f45d2de271bbe25cf11b188dff', {'production': 422}, 422),
    ('crates/rmux-core/src/grid/resize.rs', '70454e372e21c6f45d2de271bbe25cf11b188dff', {'production': 347}, 347),
    ('crates/rmux-core/src/grid/scroll.rs', '70454e372e21c6f45d2de271bbe25cf11b188dff', {'production': 124}, 124),
)


# Suite S1873: five source-runtime responsibilities, counted by the real scan
# at dc296427. Synthetic population coverage only; prior epochs,
# size caps and semantic guards stay unchanged.
SOURCE_RUNTIME_SPLIT_POPULATION_EPOCH = {
    'revision': 'dc296427952747ca0aacea768c0bb9b14ca60c44',
    'historical_population': 2624,
    'population': 2629,
}
SOURCE_RUNTIME_SPLIT_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/handler/scripting_support/source_runtime/execution.rs', 'dc296427952747ca0aacea768c0bb9b14ca60c44', {'production': 238}, 238),
    ('crates/rmux-server/src/handler/scripting_support/source_runtime/loading.rs', 'dc296427952747ca0aacea768c0bb9b14ca60c44', {'production': 344}, 344),
    ('crates/rmux-server/src/handler/scripting_support/source_runtime/parsing.rs', 'dc296427952747ca0aacea768c0bb9b14ca60c44', {'production': 201}, 201),
    ('crates/rmux-server/src/handler/scripting_support/source_runtime/startup.rs', 'dc296427952747ca0aacea768c0bb9b14ca60c44', {'production': 145, 'tests': 6}, 151),
    ('crates/rmux-server/src/handler/scripting_support/source_runtime/validation.rs', 'dc296427952747ca0aacea768c0bb9b14ca60c44', {'production': 502}, 502),
)


# S1862: attach forwarding split, measured by the real scan at the stable code parent.
# Append-only synthetic population coverage; historical epochs and semantic guards stay intact.
PANE_IO_SPLIT_POPULATION_EPOCH = {
    'revision': 'de419dbca1a39d5eea487f80b0f13ad568af880d',
    'historical_population': 2629,
    'population': 2639,
}
PANE_IO_SPLIT_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/pane_io/forward.rs', 'de419dbca1a39d5eea487f80b0f13ad568af880d', {'production': 203}, 203),
    ('crates/rmux-server/src/pane_io/forward/control.rs', 'de419dbca1a39d5eea487f80b0f13ad568af880d', {'production': 299}, 299),
    ('crates/rmux-server/src/pane_io/forward/input.rs', 'de419dbca1a39d5eea487f80b0f13ad568af880d', {'production': 67}, 67),
    ('crates/rmux-server/src/pane_io/forward/output.rs', 'de419dbca1a39d5eea487f80b0f13ad568af880d', {'production': 348, 'tests': 2}, 350),
    ('crates/rmux-server/src/pane_io/forward/prepare.rs', 'de419dbca1a39d5eea487f80b0f13ad568af880d', {'production': 261}, 261),
    ('crates/rmux-server/src/pane_io/forward/refresh.rs', 'de419dbca1a39d5eea487f80b0f13ad568af880d', {'production': 204}, 204),
    ('crates/rmux-server/src/pane_io/forward/status.rs', 'de419dbca1a39d5eea487f80b0f13ad568af880d', {'production': 125}, 125),
    ('crates/rmux-server/src/pane_io/input.rs', 'de419dbca1a39d5eea487f80b0f13ad568af880d', {'production': 230, 'tests': 24}, 254),
    ('crates/rmux-server/src/pane_io/lifecycle.rs', 'de419dbca1a39d5eea487f80b0f13ad568af880d', {'production': 288}, 288),
    ('crates/rmux-server/src/pane_io/render.rs', 'de419dbca1a39d5eea487f80b0f13ad568af880d', {'production': 183}, 183),
)


# Suite S1875, replayed on ff1823f3 (R3036): six lifecycle support modules, counted with the real
# scanner and inherited consumers. Prior epochs and semantic guards stay unchanged.
LIFECYCLE_SUPPORT_SPLIT_POPULATION_EPOCH = {
    'revision': '1432bd462d5176c0e339454905e25268bafb5ce6',
    'historical_population': 2639,
    'population': 2645,
}
LIFECYCLE_SUPPORT_SPLIT_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/handler/lifecycle_support/command_hooks.rs', '1432bd462d5176c0e339454905e25268bafb5ce6', {'production': 306}, 306),
    ('crates/rmux-server/src/handler/lifecycle_support/current_target.rs', '1432bd462d5176c0e339454905e25268bafb5ce6', {'production': 194}, 194),
    ('crates/rmux-server/src/handler/lifecycle_support/formats.rs', '1432bd462d5176c0e339454905e25268bafb5ce6', {'production': 207}, 207),
    ('crates/rmux-server/src/handler/lifecycle_support/hook_dispatch.rs', '1432bd462d5176c0e339454905e25268bafb5ce6', {'production': 190}, 190),
    ('crates/rmux-server/src/handler/lifecycle_support/publication.rs', '1432bd462d5176c0e339454905e25268bafb5ce6', {'production': 269, 'tests': 20}, 289),
    ('crates/rmux-server/src/handler/lifecycle_support/stable_target.rs', '1432bd462d5176c0e339454905e25268bafb5ce6', {'production': 397}, 397),
)


# Suite S1872: four attach delivery/render modules, counted with the real
# scanner and inherited consumers. Prior epochs and semantic guards stay unchanged.
ATTACH_SUPPORT_SPLIT_POPULATION_EPOCH = {
    'revision': '3980517d7f7e0b6bf4c91a18f2f4ec07e1ce538b',
    'historical_population': 2645,
    'population': 2649,
}
ATTACH_SUPPORT_SPLIT_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/handler/attach_support/control_delivery.rs', '3980517d7f7e0b6bf4c91a18f2f4ec07e1ce538b', {'production': 409, 'tests': 49}, 458),
    ('crates/rmux-server/src/handler/attach_support/detach_lifecycle.rs', '3980517d7f7e0b6bf4c91a18f2f4ec07e1ce538b', {'production': 180}, 180),
    ('crates/rmux-server/src/handler/attach_support/overlay_delivery.rs', '3980517d7f7e0b6bf4c91a18f2f4ec07e1ce538b', {'production': 328}, 328),
    ('crates/rmux-server/src/handler/attach_support/render_target.rs', '3980517d7f7e0b6bf4c91a18f2f4ec07e1ce538b', {'production': 498}, 498),
)


# Suite S1861: six frame-ledger sections, counted by the real scan at
# 14d92f56. Prior epochs, size caps and semantic guards stay unchanged.
FRAME_LEDGER_SPLIT_POPULATION_EPOCH = {
    'revision': '14d92f56e45ba258a36f30f180d00991641d9ad8',
    'historical_population': 2649,
    'population': 2655,
}
FRAME_LEDGER_SPLIT_POPULATION_ADDITIONS = (
    ('crates/rmux-proto/src/frame_kind/request_commands.rs', '14d92f56e45ba258a36f30f180d00991641d9ad8', {'production': 519}, 519),
    ('crates/rmux-proto/src/frame_kind/request_control.rs', '14d92f56e45ba258a36f30f180d00991641d9ad8', {'production': 330}, 330),
    ('crates/rmux-proto/src/frame_kind/request_sdk.rs', '14d92f56e45ba258a36f30f180d00991641d9ad8', {'production': 420}, 420),
    ('crates/rmux-proto/src/frame_kind/response_commands.rs', '14d92f56e45ba258a36f30f180d00991641d9ad8', {'production': 492}, 492),
    ('crates/rmux-proto/src/frame_kind/response_control.rs', '14d92f56e45ba258a36f30f180d00991641d9ad8', {'production': 231}, 231),
    ('crates/rmux-proto/src/frame_kind/response_sdk.rs', '14d92f56e45ba258a36f30f180d00991641d9ad8', {'production': 297}, 297),
)


# S1863: child modules of attached live input, measured by the real scan.
LIVE_INPUT_SPLIT_POPULATION_EPOCH = {
    'revision': 'd4d516752f614290f6e85c644a58b9f868094a49',
    'historical_population': 2655,
    'population': 2663,
}
LIVE_INPUT_SPLIT_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/handler/pane_support/pane_attached_input/live/activity.rs', 'd4d516752f614290f6e85c644a58b9f868094a49', {'production': 237}, 237),
    ('crates/rmux-server/src/handler/pane_support/pane_attached_input/live/decode.rs', 'd4d516752f614290f6e85c644a58b9f868094a49', {'production': 585}, 585),
    ('crates/rmux-server/src/handler/pane_support/pane_attached_input/live/dispatch.rs', 'd4d516752f614290f6e85c644a58b9f868094a49', {'production': 336, 'tests': 12}, 348),
    ('crates/rmux-server/src/handler/pane_support/pane_attached_input/live/fast_path.rs', 'd4d516752f614290f6e85c644a58b9f868094a49', {'production': 151}, 151),
    ('crates/rmux-server/src/handler/pane_support/pane_attached_input/live/frame_order.rs', 'd4d516752f614290f6e85c644a58b9f868094a49', {'production': 167, 'tests': 2}, 169),
    ('crates/rmux-server/src/handler/pane_support/pane_attached_input/live/modal.rs', 'd4d516752f614290f6e85c644a58b9f868094a49', {'production': 301}, 301),
    ('crates/rmux-server/src/handler/pane_support/pane_attached_input/live/windows.rs', 'd4d516752f614290f6e85c644a58b9f868094a49', {'production': 66, 'tests': 61}, 127),
    ('crates/rmux-server/src/handler/pane_support/pane_attached_input/live/work_queue.rs', 'd4d516752f614290f6e85c644a58b9f868094a49', {'production': 197}, 197),
)


# Suite S1877: isolate per-connection dispatch and its SDK wait response helper.
# One child counted by the real scan; prior epochs and guards are unchanged.
LISTENER_DISPATCH_SPLIT_POPULATION_EPOCH = {
    'revision': '8d4b2dc0655bf7079c41d824f838233d2cc4277b',
    'historical_population': 2663,
    'population': 2664,
}
LISTENER_DISPATCH_SPLIT_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/listener/connection_dispatch.rs', '8d4b2dc0655bf7079c41d824f838233d2cc4277b', {'production': 491, 'tests': 6}, 497),
)


# Suite S1878: split session creation, deferred Windows startup, kill and rename.
# Four children counted by the real scan; prior epochs and guards are unchanged.
SESSION_SUPPORT_SPLIT_POPULATION_EPOCH = {
    'revision': '4a9b4733b8c21aa8a80b4429b2597a021ffcf003',
    'historical_population': 2664,
    'population': 2668,
}
SESSION_SUPPORT_SPLIT_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/handler/session_support/creation.rs', '4a9b4733b8c21aa8a80b4429b2597a021ffcf003', {'production': 511}, 511),
    ('crates/rmux-server/src/handler/session_support/deferred.rs', '4a9b4733b8c21aa8a80b4429b2597a021ffcf003', {'production': 354, 'tests': 9}, 363),
    ('crates/rmux-server/src/handler/session_support/kill.rs', '4a9b4733b8c21aa8a80b4429b2597a021ffcf003', {'production': 515, 'tests': 9}, 524),
    ('crates/rmux-server/src/handler/session_support/rename.rs', '4a9b4733b8c21aa8a80b4429b2597a021ffcf003', {'production': 110, 'tests': 6}, 116),
)


# S1879: control support split; counts from the real scan at the introduction.
# Append-only population rows; existing epochs and guards remain unchanged.
CONTROL_SUPPORT_SPLIT_POPULATION_EPOCH = {
    'revision': 'c6de66d30c6d2055d805638b81175b3a0e2bc4af',
    'historical_population': 2668,
    'population': 2673,
}
CONTROL_SUPPORT_SPLIT_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/handler/control_support/client_lookup.rs', 'c6de66d30c6d2055d805638b81175b3a0e2bc4af', {'production': 360, 'tests': 17}, 377),
    ('crates/rmux-server/src/handler/control_support/notifications.rs', 'c6de66d30c6d2055d805638b81175b3a0e2bc4af', {'production': 323, 'tests': 25}, 348),
    ('crates/rmux-server/src/handler/control_support/pane_output.rs', 'c6de66d30c6d2055d805638b81175b3a0e2bc4af', {'production': 50, 'tests': 25}, 75),
    ('crates/rmux-server/src/handler/control_support/registration.rs', 'c6de66d30c6d2055d805638b81175b3a0e2bc4af', {'production': 259, 'tests': 59}, 318),
    ('crates/rmux-server/src/handler/control_support/session_switch.rs', 'c6de66d30c6d2055d805638b81175b3a0e2bc4af', {'production': 233, 'tests': 63}, 296),
)


# Suite S1881: three switch-client modules, counted with the real
# scanner and inherited consumers. Prior epochs and semantic guards stay unchanged.
SWITCHING_SPLIT_POPULATION_EPOCH = {
    'revision': '486398a8fdeb75954a9a2b3e4b82e2ff1e0225da',
    'historical_population': 2673,
    'population': 2676,
}
SWITCHING_SPLIT_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/handler/client_support/switching/client_identity.rs', '486398a8fdeb75954a9a2b3e4b82e2ff1e0225da', {'production': 267}, 267),
    ('crates/rmux-server/src/handler/client_support/switching/target_capture.rs', '486398a8fdeb75954a9a2b3e4b82e2ff1e0225da', {'production': 64, 'tests': 57}, 121),
    ('crates/rmux-server/src/handler/client_support/switching/target_resolution.rs', '486398a8fdeb75954a9a2b3e4b82e2ff1e0225da', {'production': 274}, 274),
)


# Suite S1882: five window support handler modules, counted by the real scan
# at f28f36f4. Prior epochs, size caps and semantic guards stay unchanged.
WINDOW_SUPPORT_SPLIT_POPULATION_EPOCH = {
    'revision': 'f28f36f469307964e0105c2c4a03158343682d2f',
    'historical_population': 2676,
    'population': 2681,
}
WINDOW_SUPPORT_SPLIT_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/handler/window_support/arrangement.rs', 'f28f36f469307964e0105c2c4a03158343682d2f', {'production': 305}, 305),
    ('crates/rmux-server/src/handler/window_support/kill.rs', 'f28f36f469307964e0105c2c4a03158343682d2f', {'production': 276, 'tests': 2}, 278),
    ('crates/rmux-server/src/handler/window_support/linking.rs', 'f28f36f469307964e0105c2c4a03158343682d2f', {'production': 462}, 462),
    ('crates/rmux-server/src/handler/window_support/navigation.rs', 'f28f36f469307964e0105c2c4a03158343682d2f', {'production': 306}, 306),
    ('crates/rmux-server/src/handler/window_support/spawn.rs', 'f28f36f469307964e0105c2c4a03158343682d2f', {'production': 267}, 267),
)


# S1883: pane I/O types split by responsibility; counts from the real scan.
# Prior epochs, semantic guards and size limits remain unchanged.
PANE_OUTPUT_TYPES_SPLIT_POPULATION_EPOCH = {
    'revision': '6e75f02d85f9fea73f138ab91d4937c36e1962bd',
    'historical_population': 2681,
    'population': 2688,
}
PANE_OUTPUT_TYPES_SPLIT_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/pane_io/types/attach.rs', '6e75f02d85f9fea73f138ab91d4937c36e1962bd', {'production': 209, 'tests': 18}, 227),
    ('crates/rmux-server/src/pane_io/types/events.rs', '6e75f02d85f9fea73f138ab91d4937c36e1962bd', {'production': 128}, 128),
    ('crates/rmux-server/src/pane_io/types/output_state.rs', '6e75f02d85f9fea73f138ab91d4937c36e1962bd', {'production': 133}, 133),
    ('crates/rmux-server/src/pane_io/types/read_demand.rs', '6e75f02d85f9fea73f138ab91d4937c36e1962bd', {'production': 262, 'tests': 8}, 270),
    ('crates/rmux-server/src/pane_io/types/receiver.rs', '6e75f02d85f9fea73f138ab91d4937c36e1962bd', {'production': 366}, 366),
    ('crates/rmux-server/src/pane_io/types/sender.rs', '6e75f02d85f9fea73f138ab91d4937c36e1962bd', {'production': 428, 'tests': 95}, 523),
    ('crates/rmux-server/src/pane_io/types/tests.rs', '6e75f02d85f9fea73f138ab91d4937c36e1962bd', {'tests': 866}, 866),
)


# S1884: five pane_io_encoding child modules, counted by the real scan at
# a2993356. Prior epochs, size caps and semantic guards stay unchanged.
PANE_IO_ENCODING_SPLIT_POPULATION_EPOCH = {
    'revision': 'a2993356b504aac56505a06f32bb8fcb5986db74',
    'historical_population': 2688,
    'population': 2693,
}
PANE_IO_ENCODING_SPLIT_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/handler/pane_support/pane_io_encoding/key_encoding.rs', 'a2993356b504aac56505a06f32bb8fcb5986db74', {'production': 127, 'tests': 100}, 227),
    ('crates/rmux-server/src/handler/pane_support/pane_io_encoding/windows_console_keys.rs', 'a2993356b504aac56505a06f32bb8fcb5986db74', {'production': 216, 'tests': 68}, 284),
    ('crates/rmux-server/src/handler/pane_support/pane_io_encoding/windows_target_shell.rs', 'a2993356b504aac56505a06f32bb8fcb5986db74', {'production': 242, 'tests': 68}, 310),
    ('crates/rmux-server/src/handler/pane_support/pane_io_encoding/write_failure.rs', 'a2993356b504aac56505a06f32bb8fcb5986db74', {'production': 155, 'tests': 112}, 267),
    ('crates/rmux-server/src/handler/pane_support/pane_io_encoding/write_io.rs', 'a2993356b504aac56505a06f32bb8fcb5986db74', {'production': 277, 'tests': 18}, 295),
)


# S1892: six test-only pane delta modules; counts from the real scan.
# Append-only population coverage; prior epochs and guards remain unchanged.
PANE_DELTA_TEST_SPLIT_POPULATION_EPOCH = {
    'revision': '1b9220b3ea3ec9c70a21c0ecf759e36f9ba00bdf',
    'historical_population': 2693,
    'population': 2699,
}
PANE_DELTA_TEST_SPLIT_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/renderer/pane_delta/tests.rs', '1b9220b3ea3ec9c70a21c0ecf759e36f9ba00bdf', {'tests': 73}, 73),
    ('crates/rmux-server/src/renderer/pane_delta/tests/cursor.rs', '1b9220b3ea3ec9c70a21c0ecf759e36f9ba00bdf', {'tests': 183}, 183),
    ('crates/rmux-server/src/renderer/pane_delta/tests/plain_output.rs', '1b9220b3ea3ec9c70a21c0ecf759e36f9ba00bdf', {'tests': 274}, 274),
    ('crates/rmux-server/src/renderer/pane_delta/tests/projection.rs', '1b9220b3ea3ec9c70a21c0ecf759e36f9ba00bdf', {'tests': 154}, 154),
    ('crates/rmux-server/src/renderer/pane_delta/tests/rows.rs', '1b9220b3ea3ec9c70a21c0ecf759e36f9ba00bdf', {'tests': 401}, 401),
    ('crates/rmux-server/src/renderer/pane_delta/tests/tail_clear.rs', '1b9220b3ea3ec9c70a21c0ecf759e36f9ba00bdf', {'tests': 349}, 349),
)


# S1894: portable Windows profile contracts; append-only population declaration.
TERMINAL_PROFILE_POPULATION_EPOCH = {
    'revision': '9a5e985108d26d924c83718d35a2c8f9946b5247',
    'historical_population': 2663,
    'population': 2665,
}
TERMINAL_PROFILE_POPULATION_ADDITIONS = (
    ('tests/support/terminal_profiles.rs', '9a5e985108d26d924c83718d35a2c8f9946b5247', {'tests': 199}, 199),
    ('tests/terminal_profile_contract.rs', '9a5e985108d26d924c83718d35a2c8f9946b5247', {'tests': 95}, 95),
)


# S1919: append-only declaration of the reviewed Unix EOF fixture.
CLOCK_EOF_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/test_shell/final_sink/unix_eof.rs',
     'f79a38ec2911845ccaf40f34e844ad220457d224', {'tests': 67}, 67),
)


# S1920: append-only declaration of the reviewed macOS PTY allocation test.
MACOS_PTY_POPULATION_ADDITIONS = (
    ('crates/rmux-pty/src/backend/macos_tests.rs',
     '4fccb05b50385d161d7d389ebeb6ec01f719333a', {'tests': 34}, 34),
)


# S1921: append-only declaration of the reviewed final-sink attach lifetime test.
ATTACH_LIFETIME_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/handler_send_keys_tests/bracketed_paste_final_sink/attach_lifetime.rs',
     'd62b8e0769bf922f121ec2c0cd951e5aee44b31d', {'tests': 60}, 60),
)


# S1925: append-only declaration of the controlled-EOF publication witness,
# introduced on top of 4cb69bd78 in the same commit as this row. S1932 rewrote
# the witness before integration; the counts follow its current length.
EOF_WITNESS_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/test_shell/final_sink/unix_eof_witness.rs',
     '4cb69bd78c22b8147e8101bfeb3ff30e24bd8148', {'tests': 122}, 122),
)


# S1924+S1928: append-only declaration of the final-sink attach guard and its
# handler-side helper, introduced on top of 7528a1aeb by the train replay.
ATTACH_GUARD_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/handler_send_keys_tests/bracketed_paste_final_sink/attached_client.rs',
     '7528a1aebd17401f1ca1dd31261bcf5458032038', {'tests': 39}, 39),
    ('crates/rmux-server/src/test_shell/final_sink/attached_client.rs',
     '7528a1aebd17401f1ca1dd31261bcf5458032038', {'tests': 90}, 90),
)


# S1966: append-only declaration of the CLI command lock long-wait test,
# introduced on the trunk by 02525f59e on top of ec3bd98be.
CLI_COMMAND_LOCK_POPULATION_ADDITIONS = (
    ('tests/cli_command_lock_long_wait.rs',
     '02525f59ec328a6ba51475847e0d5304c8cae531', {'tests': 135}, 135),
)


# S1977, extended by S1991: append-only declaration of the test sources the
# trunk added after d3e932f4e, each included by #[path]: the Linux comm
# regression module (b7680cc1c), the CLI command window helper (bccee5f6f) and
# the pane exit probe retention race (d9b519fa0).
TRUNK_TEST_POPULATION_ADDITIONS = (
    ('crates/rmux-os/src/process/linux_impl/tests.rs',
     'b7680cc1c6812629c6b1fb52e3f5052bb1a402b0', {'tests': 28}, 28),
    ('tests/common/cli_command_window.rs',
     'bccee5f6f75cb60703a674db6eef30a9522eeacb', {'tests': 49}, 49),
    ('crates/rmux-server/src/handler_pane_state_race_tests/exit_probe_retention.rs',
     'd9b519fa04e5cef3429b6a194de882e633349a29', {'tests': 141}, 141),
)


# S1962, rebased by S1966, S1977 then S1991: append-only declaration of the child modules
# introduced by the 600-line production file splits on top of d9b519fa0
# (one commit per original file).
PRODUCTION_SIZE_SPLIT_POPULATION_ADDITIONS = (
    ('crates/rmux-client/src/attach/unix/actions.rs',
     'cb36cb7f7e184340ac2c04beab3fc50707904524', {'production': 243}, 243),
    ('crates/rmux-client/src/attach/unix/output.rs',
     'cb36cb7f7e184340ac2c04beab3fc50707904524', {'production': 369, 'tests': 24}, 393),
    ('crates/rmux-client/src/attach/windows/input/keys.rs',
     '88d0bdaa9f035f53b1a51ab31e51674ffd1f77a0', {'production': 314}, 314),
    ('crates/rmux-client/src/attach/windows/input/mouse.rs',
     '88d0bdaa9f035f53b1a51ab31e51674ffd1f77a0', {'production': 136}, 136),
    ('crates/rmux-client/src/attach/windows/input/paste.rs',
     '88d0bdaa9f035f53b1a51ab31e51674ffd1f77a0', {'production': 395, 'tests': 23}, 418),
    ('crates/rmux-client/src/attach/windows/stream/input_forwarding.rs',
     '894414e6646e326cd5f5a4ef278d9be78201c5df', {'production': 91}, 91),
    ('crates/rmux-client/src/attach/windows/stream/message_drain.rs',
     '894414e6646e326cd5f5a4ef278d9be78201c5df', {'production': 149}, 149),
    ('crates/rmux-client/src/attach/windows/stream/mouse_tracker.rs',
     '894414e6646e326cd5f5a4ef278d9be78201c5df', {'production': 65}, 65),
    ('crates/rmux-client/src/attach/windows/stream/output_queue.rs',
     '894414e6646e326cd5f5a4ef278d9be78201c5df', {'production': 398, 'tests': 11}, 409),
    ('crates/rmux-client/src/attach/windows/stream/spool.rs',
     '894414e6646e326cd5f5a4ef278d9be78201c5df', {'production': 371}, 371),
    ('crates/rmux-client/src/attach/windows/terminal/console_control.rs',
     '4d0c6c91bffaf00d5474e1751489d6e72827755e', {'production': 256}, 256),
    ('crates/rmux-client/src/attach/windows/terminal/resize.rs',
     '4d0c6c91bffaf00d5474e1751489d6e72827755e', {'production': 98}, 98),
    ('crates/rmux-client/src/auto_start/error.rs',
     'a6b20ef43536e45ed271df417d56d0c10205f5e1', {'production': 205}, 205),
    ('crates/rmux-client/src/auto_start/launch.rs',
     'a6b20ef43536e45ed271df417d56d0c10205f5e1', {'production': 182, 'tests': 20}, 202),
    ('crates/rmux-client/src/auto_start/readiness.rs',
     'a6b20ef43536e45ed271df417d56d0c10205f5e1', {'production': 151}, 151),
    ('crates/rmux-core/src/options/registry/table/server.rs',
     'be0419e95569f854987ab6dcf887b58d0f5e94cf', {'production': 418}, 418),
    ('crates/rmux-core/src/options/registry/table/session.rs',
     'be0419e95569f854987ab6dcf887b58d0f5e94cf', {'production': 442}, 442),
    ('crates/rmux-core/src/options/registry/table/session_status.rs',
     'be0419e95569f854987ab6dcf887b58d0f5e94cf', {'production': 378}, 378),
    ('crates/rmux-core/src/options/registry/table/window.rs',
     'be0419e95569f854987ab6dcf887b58d0f5e94cf', {'production': 378}, 378),
    ('crates/rmux-core/src/options/registry/table/window_display.rs',
     'be0419e95569f854987ab6dcf887b58d0f5e94cf', {'production': 486}, 486),
    ('crates/rmux-core/src/screen/writer/alternate.rs',
     'b984e92ad6324b59b2623205f3a13b51ce4db383', {'production': 72}, 72),
    ('crates/rmux-core/src/screen/writer/reset.rs',
     'b984e92ad6324b59b2623205f3a13b51ce4db383', {'production': 73}, 73),
    ('crates/rmux-core/src/screen/writer/tabs.rs',
     'b984e92ad6324b59b2623205f3a13b51ce4db383', {'production': 48}, 48),
    ('crates/rmux-os/src/process/macos_impl.rs',
     '5b62c60e2c4ea9d3b7a0e3e84657580f127add2a', {'production': 263}, 263),
    ('crates/rmux-os/src/process/windows_process/job.rs',
     '708f96eb9a493d6935f852028431b3886fd933b2', {'production': 125}, 125),
    ('crates/rmux-os/src/process_tree/group_signals.rs',
     'a06b5474ec1f2a8dacf09c8db31742a77cf1fc55', {'production': 168}, 168),
    ('crates/rmux-proto/src/request/pane/output.rs',
     '5fe8d76e7f03868734c7cb94aa2ba7c56ebd86d1', {'production': 106}, 106),
    ('crates/rmux-proto/src/request/pane/sdk.rs',
     '5fe8d76e7f03868734c7cb94aa2ba7c56ebd86d1', {'production': 176}, 176),
    ('crates/rmux-proto/src/request/pane/send_keys.rs',
     '5fe8d76e7f03868734c7cb94aa2ba7c56ebd86d1', {'production': 71}, 71),
    ('crates/rmux-proto/src/request/routing.rs',
     'd91104b92f8ce8e8831b02e7c0d0d3f22444b670', {'production': 148}, 148),
    ('crates/rmux-proto/src/request/wait.rs',
     'd91104b92f8ce8e8831b02e7c0d0d3f22444b670', {'production': 72}, 72),
    ('crates/rmux-proto/src/response/buffer.rs',
     '6ca22103072725203ff890151de41e3476e4cdb0', {'production': 70}, 70),
    ('crates/rmux-proto/src/response/pane/output.rs',
     '40321b4a5c7c8b4a8185b6e24236c30ac33885f9', {'production': 95}, 95),
    ('crates/rmux-proto/src/response/pane/state.rs',
     '40321b4a5c7c8b4a8185b6e24236c30ac33885f9', {'production': 213}, 213),
    ('crates/rmux-proto/src/response/pane/stream.rs',
     '40321b4a5c7c8b4a8185b6e24236c30ac33885f9', {'production': 247}, 247),
    ('crates/rmux-proto/src/response/shell.rs',
     '6ca22103072725203ff890151de41e3476e4cdb0', {'production': 302}, 302),
    ('crates/rmux-proto/src/response/snapshot.rs',
     '6ca22103072725203ff890151de41e3476e4cdb0', {'production': 88}, 88),
    ('crates/rmux-proto/src/types/ids.rs',
     '6dcca0ed4f352b6447edf8014a1aefaf278de885', {'production': 87}, 87),
    ('crates/rmux-proto/src/types/layout.rs',
     '6dcca0ed4f352b6447edf8014a1aefaf278de885', {'production': 162}, 162),
    ('crates/rmux-pty/src/pty/io_endpoint.rs',
     '60b43f50ef6fee22bd8cbabdeb50e97971bcf90b', {'production': 268}, 268),
    ('crates/rmux-pty/src/windows_console_input/attach.rs',
     '301f4297b18fc3183fc7a770ee86e860bbbb125e', {'production': 143}, 143),
    ('crates/rmux-pty/src/windows_console_input/key_event.rs',
     '301f4297b18fc3183fc7a770ee86e860bbbb125e', {'production': 142}, 142),
    ('crates/rmux-sdk/src/events/recovery/proto_mapping.rs',
     'fd2b93ef4dcfccbcbb6b7fa771c0ad653c60f4a0', {'production': 144}, 144),
    ('crates/rmux-server/src/format_runtime/variables/format_variables.rs',
     '461d3ea0dde74a0c3b8b5cc44e9c9e96f4d34d41', {'production': 196}, 196),
    ('crates/rmux-server/src/format_runtime/variables/pane_values.rs',
     '461d3ea0dde74a0c3b8b5cc44e9c9e96f4d34d41', {'production': 202}, 202),
    ('crates/rmux-server/src/format_runtime/variables/runtime_value.rs',
     '461d3ea0dde74a0c3b8b5cc44e9c9e96f4d34d41', {'production': 59}, 59),
    ('crates/rmux-server/src/format_runtime/variables/window_stack.rs',
     '461d3ea0dde74a0c3b8b5cc44e9c9e96f4d34d41', {'production': 35}, 35),
    ('crates/rmux-server/src/handler/attach_support/resize_policy/client_removal.rs',
     '6f5562838d67fe9dc7764bb7f7fbe7f2103a3329', {'production': 95}, 95),
    ('crates/rmux-server/src/handler/attach_support/resize_policy/ranking.rs',
     '6f5562838d67fe9dc7764bb7f7fbe7f2103a3329', {'production': 115}, 115),
    ('crates/rmux-server/src/handler/attach_support/resize_policy/reconcile.rs',
     '6f5562838d67fe9dc7764bb7f7fbe7f2103a3329', {'production': 184}, 184),
    ('crates/rmux-server/src/handler/attach_support/resize_policy/selection.rs',
     '6f5562838d67fe9dc7764bb7f7fbe7f2103a3329', {'production': 276}, 276),
    ('crates/rmux-server/src/handler/construction.rs',
     '0c2dc10b2d6f7b707524e0941403a66a07e4f983', {'production': 153, 'tests': 67}, 220),
    ('crates/rmux-server/src/handler/overlay_support/clear.rs',
     'e3913cabfd306441345f7022b713678a29fa349a', {'production': 181}, 181),
    ('crates/rmux-server/src/handler/overlay_support/input_routing.rs',
     'e3913cabfd306441345f7022b713678a29fa349a', {'production': 358, 'tests': 18}, 376),
    ('crates/rmux-server/src/handler/overlay_support/popup_lifecycle.rs',
     'e3913cabfd306441345f7022b713678a29fa349a', {'production': 231, 'tests': 27}, 258),
    ('crates/rmux-server/src/handler/overlay_support/refresh.rs',
     'e3913cabfd306441345f7022b713678a29fa349a', {'production': 183}, 183),
    ('crates/rmux-server/src/handler/overlay_support/resize.rs',
     'e3913cabfd306441345f7022b713678a29fa349a', {'production': 264, 'tests': 13}, 277),
    ('crates/rmux-server/src/handler/pane_support/pane_display_panes/input_dispatch.rs',
     '044814e1915a82321d1659f7f23ef74147a4cb5f', {'production': 391}, 391),
    ('crates/rmux-server/src/handler/pane_support/pane_display_panes/overlay_lifecycle.rs',
     '044814e1915a82321d1659f7f23ef74147a4cb5f', {'production': 326, 'tests': 9}, 335),
    ('crates/rmux-server/src/handler/pane_support/pane_inspection/display_message_clients.rs',
     '7cc453067c88d4f2cbc5fa217682433dd2d158a2', {'production': 144}, 144),
    ('crates/rmux-server/src/handler/pane_support/pane_inspection/display_message_dispatch.rs',
     '7cc453067c88d4f2cbc5fa217682433dd2d158a2', {'production': 476}, 476),
    ('crates/rmux-server/src/handler/pane_support/pane_inspection/list_panes.rs',
     '7cc453067c88d4f2cbc5fa217682433dd2d158a2', {'production': 291}, 291),
    ('crates/rmux-server/src/handler/pane_support/pane_lifecycle/exit_publication.rs',
     '3e62936d401c970cbd98a0f3ee20221e5296ba5d', {'production': 226}, 226),
    ('crates/rmux-server/src/handler/pane_support/pane_lifecycle/kept_dead_pane.rs',
     '3e62936d401c970cbd98a0f3ee20221e5296ba5d', {'production': 227}, 227),
    ('crates/rmux-server/src/handler/pane_support/pane_transfer/transfer_effects.rs',
     'ce95930138a4f5a85e0286b03b6a38cff3a10279', {'production': 376}, 376),
    ('crates/rmux-server/src/handler/pane_support/pane_transfer/window_identity.rs',
     'ce95930138a4f5a85e0286b03b6a38cff3a10279', {'production': 139}, 139),
    ('crates/rmux-server/src/handler/requester_access.rs',
     '0c2dc10b2d6f7b707524e0941403a66a07e4f983', {'production': 107}, 107),
    ('crates/rmux-server/src/handler/scripting_support/shell_runtime/if_shell.rs',
     '3c74e83e396ecc7dbf34c929016a9944647100e3', {'production': 259}, 259),
    ('crates/rmux-server/src/handler/scripting_support/shell_runtime/if_shell_queue.rs',
     '3c74e83e396ecc7dbf34c929016a9944647100e3', {'production': 351}, 351),
    ('crates/rmux-server/src/handler/scripting_support/shell_runtime/run_shell.rs',
     '3c74e83e396ecc7dbf34c929016a9944647100e3', {'production': 473}, 473),
    ('crates/rmux-server/src/handler/scripting_support/shell_runtime/run_shell_preparation.rs',
     '3c74e83e396ecc7dbf34c929016a9944647100e3', {'production': 228, 'tests': 3}, 231),
    ('crates/rmux-server/src/handler/scripting_support/targets/new_window.rs',
     '9ca4e277bed742c0fa82e74034205c44c14a0dd2', {'production': 184}, 184),
    ('crates/rmux-server/src/handler/scripting_support/targets/queue_arguments.rs',
     '9ca4e277bed742c0fa82e74034205c44c14a0dd2', {'production': 448}, 448),
    ('crates/rmux-server/src/handler/shared_handles.rs',
     '0c2dc10b2d6f7b707524e0941403a66a07e4f983', {'production': 267, 'tests': 98}, 365),
    ('crates/rmux-server/src/pane_terminals/pane_transcripts/screen_access.rs',
     'b7c2b2737b341d4e014d321c72254cf29a9ddb47', {'production': 345}, 345),
    ('crates/rmux-server/src/pane_terminals/pane_transcripts/transcript_policy.rs',
     'b7c2b2737b341d4e014d321c72254cf29a9ddb47', {'production': 290}, 290),
    ('crates/rmux-server/src/pane_transcript/capture.rs',
     '3d5f6af34e5af48e3ba892fc108db0cb490952e3', {'production': 133, 'tests': 6}, 139),
    ('crates/rmux-server/src/pane_transcript/modes.rs',
     '3d5f6af34e5af48e3ba892fc108db0cb490952e3', {'production': 205}, 205),
    ('crates/rmux-server/src/pane_transcript/timers.rs',
     '3d5f6af34e5af48e3ba892fc108db0cb490952e3', {'production': 142, 'tests': 13}, 155),
    ('src/cli/claude_launcher/attached.rs',
     'd343203d13a7820c0c4c38683dfe87a29654ceee', {'production': 432}, 432),
    ('src/cli/claude_launcher/claude_program.rs',
     'd343203d13a7820c0c4c38683dfe87a29654ceee', {'production': 162}, 162),
    ('src/cli/claude_launcher/runtime_dir.rs',
     'd343203d13a7820c0c4c38683dfe87a29654ceee', {'production': 311}, 311),
    ('src/cli/claude_launcher/tmux_shim.rs',
     'd343203d13a7820c0c4c38683dfe87a29654ceee', {'production': 402}, 402),
    ('src/cli/dispatch/command.rs',
     '37f3b1b6ac1c7a70556684e5195e399b34379dbc', {'production': 494}, 494),
    ('src/cli/window_commands/destination.rs',
     'c2c3bc198c66eb9e0d9f318b380f5699b0f08b15', {'production': 364}, 364),
    ('src/cli/window_commands/list_windows.rs',
     'c2c3bc198c66eb9e0d9f318b380f5699b0f08b15', {'production': 289}, 289),
    ('src/cli/window_commands/new_window.rs',
     'c2c3bc198c66eb9e0d9f318b380f5699b0f08b15', {'production': 271}, 271),
)


# S2020: append-only declaration of the child modules introduced on top of
# b1006e624 by the splits of the last two 600-line production files
# (one commit per original file).
PRODUCTION_SIZE_CLOSURE_POPULATION_ADDITIONS = (
    ('crates/rmux-pty/examples/conpty_spike/cli.rs',
     '80e69cd7fad5a7c1cdc52fa4de11f7f4a44f59c1', {'production': 126}, 126),
    ('crates/rmux-pty/examples/conpty_spike/win32.rs',
     '80e69cd7fad5a7c1cdc52fa4de11f7f4a44f59c1', {'production': 310}, 310),
    ('crates/rmux-server/src/handler/scripting_support/command_execution/dispatched_request.rs',
     'e6c8b39a30d5ddd8f950b93755ed1a6b47f09d94', {'production': 186}, 186),
)


# S2030: append-only declaration of the two test sources pile0045 added on the
# trunk without declaring them: the Darwin PTY close-on-exec integration test
# (1c3a0d439) and the live attached-input work probe (ff3b3dc31). Counts
# measured by inventory.py at af4e6256c; neither carries a semantic attribute.
PILE0045_TEST_POPULATION_ADDITIONS = (
    ('crates/rmux-pty/tests/pty_pair_cloexec.rs',
     '1c3a0d43982f971d0b4ad7ed0c8abb9a1630a173', {'tests': 101}, 101),
    ('crates/rmux-server/src/handler/pane_support/pane_attached_input/live/work_probe.rs',
     'ff3b3dc3162ee9a0d6ca0da6123f3bd2c41d7a35', {'tests': 66}, 66),
)


# S2030: append-only declaration of the eight sources pile0145 adds on top of
# 19b585101 (s2017r, s1980r6, s2022r), each with the pile0145 commit that
# introduces it. Counts measured by inventory.py at e45ba0102; none carries a
# semantic attribute.
PILE0145_POPULATION_ADDITIONS = (
    ('crates/rmux-pty/src/backend/unix_exit.rs',
     '17d39fbd4da11d297d4773b0db5032c9ef36467c', {'production': 35}, 35),
    ('crates/rmux-sdk/src/handles/rmux/connect/unix_startup.rs',
     'eba2f4c70ae20ab53a8f973e7aacdabdb2470c35', {'production': 136, 'tests': 170}, 306),
    ('crates/rmux-bootstrap/src/startup_unix/reservation.rs',
     '3c0b0d633564ffa6aa0d9c17f1ef93e6acbda940', {'production': 51}, 51),
    ('crates/rmux-server/src/listener/endpoint_release.rs',
     '3c0b0d633564ffa6aa0d9c17f1ef93e6acbda940', {'production': 63}, 63),
    ('crates/rmux-server/tests/idle_exit_endpoint.rs',
     '3c0b0d633564ffa6aa0d9c17f1ef93e6acbda940', {'tests': 145}, 145),
    ('crates/rmux-sdk/src/handles/pane/exit_probe.rs',
     'd04614214ad9afcab174f2c1a78c993d8076c5ad', {'production': 83, 'tests': 30}, 113),
    ('src/cli/automation/wait/quiet_race_tests.rs',
     '1b1c90e1d2c6270aa98ee5bbd1285fcff9379622', {'tests': 240}, 240),
    ('tests/cli_surface/control_error_message.rs',
     'f10f642214800ef01a244cc4887351c3c7d4e78c', {'tests': 93}, 93),
)


# S2042: append-only declaration of the two sources s2031 (pile0425b) adds: the
# macOS grantpt watchdog and its unit tests, both introduced by 30a155020. Counts
# measured by inventory.py at bc601bd12. macos_grant.rs carries only a path
# attribute classified as tests (#[path = "macos_grant_tests.rs"] mod tests;),
# which the semantic guard excludes, as for macos.rs; no guarded attribute.
S2031_POPULATION_ADDITIONS = (
    ('crates/rmux-pty/src/backend/macos_grant.rs',
     '30a155020cf5f3978c390d06f56406feb901dc54', {'production': 238, 'tests': 3}, 241),
    ('crates/rmux-pty/src/backend/macos_grant_tests.rs',
     '30a155020cf5f3978c390d06f56406feb901dc54', {'tests': 147}, 147),
)


# S2048: append-only declaration of the control attach-notice order test that
# s1989r (2ffe1b298, replayed on 9c4dc8db7 as d0c0c1458) adds under
# control::tests. Counts measured by inventory.py at 23aba7af6; it carries no
# semantic attribute.
CONTROL_ATTACH_NOTICE_TEST_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/control/tests/attach_notice_order.rs',
     'd0c0c14580beacf45567ce2c995eabf1b569dfa5', {'tests': 123}, 123),
)


# S2048: append-only declaration of the three argument_check modules that
# s1988r (dc957c6ab, replayed on 8cea4fd95 as 936f5b2df) adds under
# rmux-core's command_parser: the args_parse checker, its tmux 3.7b templates
# and its unit tests. Counts measured by inventory.py at 936f5b2df; none
# carries a semantic attribute.
ARGUMENT_CHECK_POPULATION_ADDITIONS = (
    ('crates/rmux-core/src/command_parser/argument_check.rs',
     '936f5b2df61c906d74e2f2e04b24c31884e8aa3e', {'production': 156, 'tests': 2}, 158),
    ('crates/rmux-core/src/command_parser/argument_check/templates.rs',
     '936f5b2df61c906d74e2f2e04b24c31884e8aa3e', {'production': 99}, 99),
    ('crates/rmux-core/src/command_parser/argument_check/tests.rs',
     '936f5b2df61c906d74e2f2e04b24c31884e8aa3e', {'tests': 104}, 104),
)


# S2045: append-only declaration of the Windows test-only DECRQM terminal
# module that s2032s (a7d2bcac2) adds under console_mode_ownership_tests and
# that s2045 (aab0c1628, replayed on cace8c2bc as ebc4eb533) brings to the
# trunk; S2045 extends it. Counts measured by inventory.py at 253801e68; it
# carries no semantic attribute.
CONPTY_DECRQM_TEST_POPULATION_ADDITIONS = (
    ('crates/rmux-client/src/attach_windows/console_mode_decrqm_terminal.rs',
     'ebc4eb5338cebe6112adf96dbd79bb64c45a2b21', {'tests': 239}, 239),
)


# S2056: append-only declaration of the SDK live-details module that the PR253
# port (d1cf6e9f4, on 0b8c23c16) adds under rmux-sdk's pane info: the shared
# PaneInfo mapping of the 18-field details probe and the window-scoped
# decoration. Counts measured by inventory.py at the port head; it carries no
# semantic attribute.
SDK_LIVE_DETAILS_POPULATION_ADDITIONS = (
    ('crates/rmux-sdk/src/handles/pane/info/live_details.rs',
     'd1cf6e9f4e1f7b6079c8b8245498a5c6aa07e49b', {'production': 91}, 91),
)


# S2307: append-only declaration of the 8 sources that the round-two
# batches carried from the trunk add on the r6' source. Each row names the
# carried commit that adds the file; the counts are those the trunk declares
# for the same blobs. None carries a semantic attribute.
R7_SOURCE_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/handler_attach_tests/client_last_session.rs',
     '7d2db1b41317b7ec0a04660a0650b2e5cb0d1fc0', {'tests': 338}, 338),
    ('crates/rmux-server/src/handler_tests/poisoned_transcript_named_commands.rs',
     '0555227763f1459ea95073ff9eb24123294e5595', {'tests': 284}, 284),
    ('crates/rmux-server/src/handler_tests/poisoned_transcript_transfer.rs',
     '9aeca5c1c4c349edd90dc023bcac2668be480acb', {'tests': 254}, 254),
    ('crates/rmux-server/src/pane_terminals/pane_transcripts/availability.rs',
     '9aeca5c1c4c349edd90dc023bcac2668be480acb', {'production': 55}, 55),
    ('tests/support/rerun_exact.rs',
     '0275ca9140098a8bc3320bb129008650f54d9688', {'tests': 160}, 160),
    ('crates/rmux-server/src/handler_tests/poisoned_window_selection.rs',
     '217c626ff808c3287427e223448311568b04d134', {'tests': 345}, 345),
    ('crates/rmux-server/src/listener_read_only_format_job_tests.rs',
     '26b151c86c3b8b2747604b7586c073a79009b753', {'tests': 184}, 184),
    ('crates/rmux-server/src/handler_web_destroy_rehome_tests.rs',
     '7267c0179450986b0edd4d75884348c494f568e6', {'tests': 255}, 255),
)


# S2279: append-only declaration of the source that the attach server-exit
# fix adds. The row names the commit that adds the file; its counts are
# measured by inventory.py at that branch's head before this declaration.
# It carries no semantic attribute.
S2279_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/pane_io/tests/server_exit.rs',
     'fa009e187b70723d2972df3521ad56d7c03feb78', {'tests': 163}, 163),
)


# S2270 posed on the H5 trunk: append-only declaration of the two sources
# that the issue 29 and issue 36 regression tests add. The rows name the
# commit that adds the files; their counts are measured by inventory.py at
# that commit. They carry no semantic attribute.
S2270_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/handler_attach_tests/issue36_split_client_context.rs',
     '617fc05eee95077346d8e16e3e7d5d1ad92f192a', {'tests': 73}, 73),
    ('crates/rmux-server/src/pane_io/live_render/issue29_tests.rs',
     '617fc05eee95077346d8e16e3e7d5d1ad92f192a', {'tests': 149}, 149),
)


# S2296: append-only declaration of the source that the pane selection
# regression test adds, carried onto the trunk. The row names the carried
# commit that adds the file; its counts are measured by inventory.py at that
# commit before this declaration. It carries no semantic attribute.
S2296_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/handler_tests/poisoned_pane_selection.rs',
     'b0c2dd1a7d6ab1a8347546351ab07b31f359e194', {'tests': 372}, 372),
)


# S2301: append-only declaration of the sources that the read-only control
# client fix adds on the trunk. Each row names the commit that adds the file;
# its counts are measured by inventory.py at that branch's head before this
# declaration. They carry no semantic attribute.
S2301_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/handler/pane_support/pane_send_keys/read_only.rs',
     '3239067b394a6875113de0a8cf4ad5f67a0fff0f', {'production': 41}, 41),
    ('crates/rmux-server/src/handler_client/control_read_only_tests.rs',
     '3239067b394a6875113de0a8cf4ad5f67a0fff0f', {'tests': 183}, 183),
)


# S2324: append-only declaration of the source that the control-mode
# handshake split adds on the trunk. The row names the commit that adds the
# file; its counts are measured by inventory.py at that commit before this
# declaration. It carries no semantic attribute.
S2324_POPULATION_ADDITIONS = (
    ('crates/rmux-client/src/control/handshake.rs',
     'a030e3e4a12fccbc9d2e95545a88b9531c607fd9', {'production': 79}, 79),
)


# Append-only declaration of the source that the shutdown connection refusal
# adds above the stop-block series. The row names the commit that adds the
# file; its counts are measured by inventory.py at the branch head before this
# declaration. It carries no semantic attribute.
SHUTDOWN_REFUSAL_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/listener/shutdown_refusal.rs',
     '2ce58f635ba253049172cbe2b54b1119e521de25', {'production': 58, 'tests': 49}, 107),
)


# S2312: append-only declaration of the test source that the control client
# flag fixes (attach-session -f, switch-client -r) add on the trunk. The row
# names the commit that adds the file; its counts are measured by inventory.py
# at that branch's head before this declaration. No semantic attribute.
S2312_POPULATION_ADDITIONS = (
    ('crates/rmux-server/src/handler_client/control_flag_change_tests.rs',
     '030199c8bffd14ba927bbd6a5d2c216a6337f5a6', {'tests': 404}, 404),
)


# S2343: append-only declaration of the source that the auto-start polling
# split adds on the trunk. The row names the commit that adds the file; its
# counts are measured by inventory.py at that commit before this declaration.
# It carries no semantic attribute.
S2343_POPULATION_ADDITIONS = (
    ('crates/rmux-client/src/auto_start/polling.rs',
     '662d288c45f78e7693d8f33462cd334b3a1325d4', {'production': 174}, 174),
)


# The declarations above are frozen; the S2372 sum below closes on its own line
# and no row may be added to it (FROZEN_POPULATION_SHA256 pins it). A lot that
# adds a .rs file appends one row to population-additions.jsonl instead: the
# file is merged with merge=union (.gitattributes), so two neighbouring
# declarations are pure additions that cherry-pick without conflict.
FROZEN_POPULATION_ADDITIONS = (
    POPULATION_ADDITIONS
    + COMPOSED_POPULATION_ADDITIONS
    + LOT13_POPULATION_ADDITIONS
    + CURRENT_POPULATION_ADDITIONS
    + DESCENDANT_POPULATION_ADDITIONS
    + SIZE_SPLIT_POPULATION_ADDITIONS
    + WRITER_METADATA_POPULATION_ADDITIONS
    + MODULE_SPLIT_POPULATION_ADDITIONS
    + TRUNK_SPLIT_POPULATION_ADDITIONS
    + REQUEST_PARSE_SPLIT_POPULATION_ADDITIONS
    + CLI_ARGS_PANE_SPLIT_POPULATION_ADDITIONS
    + ATTACHED_INPUT_SPLIT_POPULATION_ADDITIONS
    + GRID_SPLIT_POPULATION_ADDITIONS
    + SOURCE_RUNTIME_SPLIT_POPULATION_ADDITIONS
    + PANE_IO_SPLIT_POPULATION_ADDITIONS
    + LIFECYCLE_SUPPORT_SPLIT_POPULATION_ADDITIONS
    + ATTACH_SUPPORT_SPLIT_POPULATION_ADDITIONS
    + FRAME_LEDGER_SPLIT_POPULATION_ADDITIONS
    + LIVE_INPUT_SPLIT_POPULATION_ADDITIONS
    + LISTENER_DISPATCH_SPLIT_POPULATION_ADDITIONS
    + SESSION_SUPPORT_SPLIT_POPULATION_ADDITIONS
    + CONTROL_SUPPORT_SPLIT_POPULATION_ADDITIONS
    + SWITCHING_SPLIT_POPULATION_ADDITIONS
    + WINDOW_SUPPORT_SPLIT_POPULATION_ADDITIONS
    + PANE_OUTPUT_TYPES_SPLIT_POPULATION_ADDITIONS
    + PANE_IO_ENCODING_SPLIT_POPULATION_ADDITIONS
    + PANE_DELTA_TEST_SPLIT_POPULATION_ADDITIONS
    + TERMINAL_PROFILE_POPULATION_ADDITIONS
    + CLOCK_EOF_POPULATION_ADDITIONS
    + MACOS_PTY_POPULATION_ADDITIONS
    + ATTACH_LIFETIME_POPULATION_ADDITIONS
    + EOF_WITNESS_POPULATION_ADDITIONS
    + ATTACH_GUARD_POPULATION_ADDITIONS
    + CLI_COMMAND_LOCK_POPULATION_ADDITIONS
    + PRODUCTION_SIZE_SPLIT_POPULATION_ADDITIONS
    + TRUNK_TEST_POPULATION_ADDITIONS
    + PRODUCTION_SIZE_CLOSURE_POPULATION_ADDITIONS
    + PILE0045_TEST_POPULATION_ADDITIONS
    + PILE0145_POPULATION_ADDITIONS
    + S2031_POPULATION_ADDITIONS
    + CONTROL_ATTACH_NOTICE_TEST_POPULATION_ADDITIONS
    + ARGUMENT_CHECK_POPULATION_ADDITIONS
    + CONPTY_DECRQM_TEST_POPULATION_ADDITIONS
    + SDK_LIVE_DETAILS_POPULATION_ADDITIONS
    + R7_SOURCE_POPULATION_ADDITIONS
    + S2279_POPULATION_ADDITIONS
    + S2270_POPULATION_ADDITIONS
    + S2296_POPULATION_ADDITIONS
    + S2301_POPULATION_ADDITIONS
    + S2324_POPULATION_ADDITIONS
    + SHUTDOWN_REFUSAL_POPULATION_ADDITIONS
    + S2312_POPULATION_ADDITIONS
    + S2343_POPULATION_ADDITIONS
)
FROZEN_POPULATION_ROWS = 525
FROZEN_POPULATION_SHA256 = '172c78b1d4753ac3bda989a11fc4579e0f4f4997f1f3572af0df02fb6f0f3915'
DECLARED_POPULATION = Path(__file__).with_name('population-additions.jsonl')
DECLARED_ROW_KEYS = ('path', 'introduced_commit', 'counts', 'physical_lines')
COUNT_KINDS = ('production', 'tests', 'tooling')


def frozen_population_digest(rows):
    import hashlib
    import json
    return hashlib.sha256(json.dumps([list(row) for row in rows], sort_keys=True,
                                     separators=(',', ':')).encode()).hexdigest()


def parse_declared_population(data):
    """Rows of population-additions.jsonl: one JSON object per line, '#' lines are comments."""
    import json
    rows = []
    for number, line in enumerate(data.decode('utf-8').splitlines(), 1):
        if line.startswith('#'):
            continue
        try:
            row = json.loads(line)
        except ValueError:
            raise ValueError(f'population declaration line {number} is not one JSON object') from None
        if not isinstance(row, dict) or sorted(row) != sorted(DECLARED_ROW_KEYS):
            raise ValueError(f'population declaration line {number} must have exactly '
                             + ', '.join(DECLARED_ROW_KEYS))
        path, commit, counts, physical_lines = (row[key] for key in DECLARED_ROW_KEYS)
        if (not isinstance(path, str) or not path.endswith('.rs')
                or not isinstance(commit, str) or not re.fullmatch('[0-9a-f]{40}', commit)
                or not isinstance(counts, dict) or not counts
                or any(kind not in COUNT_KINDS or type(value) is not int or value <= 0
                       for kind, value in counts.items())
                or type(physical_lines) is not int or sum(counts.values()) != physical_lines):
            raise ValueError(f'population declaration line {number} is malformed: {path}')
        rows.append((path, commit, counts, physical_lines))
    return tuple(rows)


def declared_population():
    return parse_declared_population(DECLARED_POPULATION.read_bytes())


def introduction_refusal(repo, head, path, commit):
    """Why ``commit`` is not a commit below ``head`` that adds ``path``; None when it is.

    S2372: checked for every row of population-additions.jsonl, never for the frozen
    rows, whose historical hexes predate this gate.
    """
    resolved = subprocess.run(['git', '-C', str(repo), 'rev-parse', '-q', '--verify',
                               commit + '^{commit}'], capture_output=True)
    if resolved.returncode != 0 or resolved.stdout.decode().strip() != commit:
        return 'does not resolve to a commit'
    ancestry = subprocess.run(['git', '-C', str(repo), 'merge-base', '--is-ancestor', commit, head],
                              capture_output=True)
    if ancestry.returncode == 1:
        return 'is not an ancestor of ' + head
    if ancestry.returncode != 0:
        raise ValueError('cannot validate the ancestry of ' + commit)
    parents = git(repo, 'rev-list', '--parents', '-n', '1', commit).decode().split()[1:]
    added = git(repo, *(['diff', parents[0], commit] if parents else ['diff-tree', '--root', '-r',
                                                                    '--no-commit-id', commit]),
                '--name-only', '--no-renames', '--diff-filter=A', '--', path).decode().splitlines()
    if added != [path]:
        return 'does not add ' + path
    return None


def append_population_fixture(report, tree):
    """Add only declared rows; keep historical rows and canaries intact."""
    existing = {row['path'] for row in report['files']}
    for path, introduced_commit, counts, physical_lines in (
            FROZEN_POPULATION_ADDITIONS + declared_population()):
        if path in existing:
            raise ValueError('population fixture addition collides: ' + path)
        report['files'].append(dict(path=path, blob=tree[path]['blob'],
            counts=copy.deepcopy(counts), physical_lines=physical_lines,
            semantic_attributes=[], consumers=[]))


def admit_population(report, admitted):
    """Give the synthetic rows exactly ``admitted``, rewriting only the paths that differ.

    Path attributes classified as tests are outside the guard and stay as recorded.
    """
    import json
    rows = {row['path']: row for row in report['files']}
    current = semantic_identities(rows)
    for path in sorted({json.loads(key)[0] for key in (current - admitted) + (admitted - current)}):
        keys = [(json.loads(key), count) for key, count in admitted.items()
                if json.loads(key)[0] == path]
        consumer_lists = {json.dumps(groups, sort_keys=True) for (_, _, groups), _ in keys}
        if path not in rows or len(consumer_lists) > 1:
            raise ValueError('admitted population is not representable on ' + path)
        row = rows[path]
        row['semantic_attributes'] = [
            attribute for attribute in row['semantic_attributes']
            if attribute['kind'] == 'path' and attribute['classification'] == 'tests'
        ] + [dict(attribute) for (_, attribute, _), count in keys for _ in range(count)]
        if keys:
            row['consumers'] = keys[0][0][2]
    if semantic_identities(rows) != admitted:
        raise ValueError('synthetic population differs from the admitted population')


def synthetic_reports(repo, record, revision, trees):
    """Reports for base -> ``revision``: recorded rows, declared population, admitted semantics.

    The descendant carries the one population admitted on ``revision`` (S1799), so a
    descendant of an activation is built from the activated population, never the proved one.
    """
    reports = []
    for side, report_revision, tree in zip(('before', 'after'), (record['base'], revision), trees):
        reports.append(dict(revision=report_revision, semantic_attributes_schema=1,
            manifests=copy.deepcopy(record['manifests_' + side]),
            files=[dict(path=p, blob=tree[p]['blob'], counts=copy.deepcopy(row['counts']),
                physical_lines=row['physical_lines'],
                semantic_attributes=copy.deepcopy(row['attributes']),
                consumers=copy.deepcopy(row['consumers'])) for p, row in record[side].items()]))
    append_population_fixture(reports[1], trees[1])
    admit_population(reports[1], admitted_semantics(repo, revision, record))
    return reports


class DeclaredPopulationTests(unittest.TestCase):
    """S2372: the frozen sum stays closed; new rows live one per line in a union-merged file."""

    def test_frozen_declarations_are_closed(self):
        self.assertEqual(len(FROZEN_POPULATION_ADDITIONS), FROZEN_POPULATION_ROWS)
        self.assertEqual(frozen_population_digest(FROZEN_POPULATION_ADDITIONS),
                         FROZEN_POPULATION_SHA256)

    def test_declared_rows_are_new_unique_paths(self):
        frozen = {row[0] for row in FROZEN_POPULATION_ADDITIONS}
        paths = [row[0] for row in declared_population()]
        self.assertEqual(len(paths), len(set(paths)))
        self.assertEqual(sorted(frozen.intersection(paths)), [])

    def test_declaration_file_is_union_merged(self):
        repo = Path(__file__).resolve().parents[2]
        path = DECLARED_POPULATION.relative_to(repo).as_posix()
        attribute = git(repo, 'check-attr', 'merge', '--', path).decode()
        self.assertEqual(attribute, path + ': merge: union\n')

    def test_parser_reads_rows_and_refuses_malformed_lines(self):
        good = ('{"path": "crates/a/src/b.rs", "introduced_commit": "' + 'a' * 40
                + '", "counts": {"production": 3, "tests": 2}, "physical_lines": 5}')
        self.assertEqual(parse_declared_population(('# comment\n' + good + '\n').encode()),
                         (('crates/a/src/b.rs', 'a' * 40, {'production': 3, 'tests': 2}, 5),))
        for bad in ('', 'not json', '[1]', good.replace('"physical_lines": 5', '"physical_lines": 6'),
                    good.replace('b.rs', 'b.py'), good.replace('a' * 40, 'a' * 39),
                    good.replace('a' * 40, 'A' * 40), good.replace('"tests"', '"docs"'),
                    good.replace('"tests": 2', '"tests": 0').replace(': 5}', ': 3}'),
                    good.replace('{"path"', '{"lot": "x", "path"')):
            with self.subTest(line=bad), self.assertRaises(ValueError):
                parse_declared_population((good + '\n' + bad + '\n').encode())


class IntroductionGateTests(unittest.TestCase):
    """S2372: a declared hex must be an ancestor of HEAD that adds the declared path."""

    def test_refuses_every_commit_that_does_not_add_the_path_below_head(self):
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory)
            subprocess.run(['git', 'init', '-q', str(fixture)], check=True)

            def commit(files, *parents):
                for name, data in files.items():
                    (fixture / name).write_text(data)
                    git(fixture, 'add', name)
                tree = git(fixture, 'write-tree').decode().strip()
                env = dict(os.environ, GIT_AUTHOR_NAME='Sidney Sissaoui',
                           GIT_AUTHOR_EMAIL='shideneyu@gmail.com', GIT_COMMITTER_NAME='Sidney Sissaoui',
                           GIT_COMMITTER_EMAIL='shideneyu@gmail.com')
                parent_args = [arg for parent in parents for arg in ('-p', parent)]
                return subprocess.check_output(['git', '-C', str(fixture), 'commit-tree', tree,
                                                *parent_args, '-m', 'fixture'], env=env).decode().strip()

            root = commit({'a.rs': 'a\n'})
            head = commit({'a.rs': 'a2\n', 'b.rs': 'b\n'}, root)
            git(fixture, 'read-tree', root)
            side = commit({'c.rs': 'c\n'}, root)
            root_tree = git(fixture, 'rev-parse', root + '^{tree}').decode().strip()
            cases = {
                ('a.rs', root): None,
                ('b.rs', head): None,
                ('a.rs', head): 'does not add a.rs',
                ('b.rs', root): 'does not add b.rs',
                ('c.rs', side): 'is not an ancestor of ' + head,
                ('a.rs', '0' * 40): 'does not resolve to a commit',
                ('a.rs', root_tree): 'does not resolve to a commit',
            }
            for (path, hexadecimal), expected in cases.items():
                with self.subTest(path=path, commit=hexadecimal):
                    self.assertEqual(introduction_refusal(fixture, head, path, hexadecimal), expected)


class DescendantTransportTests(unittest.TestCase):
    def test_neutral_client_comment_preserves_relocation_transport(self):
        repo = Path(__file__).resolve().parents[2]
        record = load_chain(repo)
        audit = repo / '.rmux-audit/s2139/canaries'
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit) as directory:
            fixture = Path(directory)
            subprocess.run(['git', 'init', '-q', str(fixture)], check=True)
            common = git(repo, 'rev-parse', '--git-common-dir').decode().strip()
            objects = (repo / common / 'objects').resolve()
            borrow_objects(fixture / '.git', objects)
            parent = git(repo, 'rev-parse', 'HEAD').decode().strip()
            git(fixture, 'read-tree', parent)
            path = 'crates/rmux-client/src/lib.rs'
            data = git(repo, 'show', parent + ':' + path) + b'// neutral descendant comment\n'
            blob = subprocess.check_output(['git', '-C', str(fixture), 'hash-object',
                                             '-w', '--stdin'], input=data).decode().strip()
            git(fixture, 'update-index', '--cacheinfo', '100644,' + blob + ',' + path)
            tree = git(fixture, 'write-tree').decode().strip()
            env = dict(os.environ, GIT_AUTHOR_NAME='Sidney Sissaoui',
                       GIT_AUTHOR_EMAIL='shideneyu@gmail.com', GIT_COMMITTER_NAME='Sidney Sissaoui',
                       GIT_COMMITTER_EMAIL='shideneyu@gmail.com')
            descendant = subprocess.check_output(['git', '-C', str(fixture), 'commit-tree',
                tree, '-p', parent, '-m', 'neutral client comment canary'], env=env).decode().strip()
            git(fixture, 'update-ref', 'HEAD', descendant)
            trees = [read_tree(fixture, record['base']), read_tree(fixture, descendant)]
            reports = synthetic_reports(fixture, record, descendant, trees)
            row = next(row for row in reports[1]['files'] if row['path'] == path)
            row['physical_lines'] += 1
            row['counts']['production'] += 1
            mapping, sizes = transport(fixture, *reports, *trees, epoch=EPOCH)
            self.assertTrue(mapping)
            self.assertEqual(len(sizes), 51)


class DescendantRatchetTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.repo = Path(__file__).resolve().parents[2]
        cls.record = load_chain(cls.repo)
        cls.revision = git(cls.repo, 'rev-parse', 'HEAD').decode().strip()
        cls.trees = [read_tree(cls.repo, cls.record['base']), read_tree(cls.repo, cls.revision)]
        cls.reports = synthetic_reports(cls.repo, cls.record, cls.revision, cls.trees)

    def transport(self, reports=None, trees=None):
        return transport(self.repo, *(reports or self.reports), *(trees or self.trees), epoch=EPOCH)

    def test_declared_rows_name_the_commit_that_adds_their_path(self):
        refusals = [(path, commit, reason) for path, commit, _, _ in declared_population()
                    for reason in (introduction_refusal(self.repo, self.revision, path, commit),)
                    if reason]
        self.assertEqual(refusals, [])

    def assert_intact_transports(self):
        # Positive control (S1817): the unmutated fixture transports, so a guard
        # that refuses everything fails here instead of passing a refusal test.
        mapping, sizes = self.transport(copy.deepcopy(self.reports))
        self.assertTrue(mapping)
        self.assertEqual(len(sizes), 51)
        self.intact_transports = True

    def assert_refused(self, reason, reports, trees=None):
        """Refuse exactly ``reason``, after the intact control of this test passed."""
        if not getattr(self, 'intact_transports', False):
            self.assert_intact_transports()
        with self.assertRaisesRegex(ValueError, '^' + re.escape(reason) + '$'):
            self.transport(reports, trees)

    def test_synthetic_fixture_covers_exact_descendant_population(self):
        rows = self.reports[1]['files']
        paths = [row['path'] for row in rows]
        self.assertEqual(len(paths), len(set(paths)))
        self.assertEqual(set(paths), {p for p in self.trees[1] if p.endswith('.rs')})
        for row in rows:
            self.assertEqual(row['blob'], self.trees[1][row['path']]['blob'])
        # Canary: HEAD descends from the activation, so the fixture carries the activated
        # population; a fixture copying the proved one would be refused by transport.
        from relocation_chain import proved_semantics
        identities = semantic_identities({row['path']: row for row in rows})
        self.assertEqual(identities, admitted_semantics(self.repo, self.revision, self.record))
        self.assertNotEqual(identities, proved_semantics(self.record))
        mapping, sizes = self.transport()
        self.assertTrue(mapping)
        self.assertEqual(sizes, {item['new']: item['old'] for item in self.record['sizes']})

    def test_descendant_semantic_removal_substitution_cfg_and_consumers_refuse(self):
        # Positive control (R2980): the intact copy passes and each mutation changes the
        # guarded identities, so a fixture the guard already refuses cannot pass vacuously.
        self.assert_intact_transports()
        intact_identities = semantic_identities({row['path']: row for row in self.reports[1]['files']})
        for kind, classification in (('allow_dead_code', None), ('path', 'tooling')):
            for mutation in ('add', 'remove', 'substitute', 'cfg', 'consumer', 'consumer_remove'):
                with self.subTest(kind=kind, mutation=mutation):
                    reports = copy.deepcopy(self.reports)
                    row, attribute = next((r, a) for r in reports[1]['files']
                        for a in r['semantic_attributes'] if a['kind'] == kind and
                        (classification is None or a['classification'] == classification))
                    if mutation == 'add':
                        row['semantic_attributes'].append(copy.deepcopy(attribute))
                    elif mutation == 'remove':
                        row['semantic_attributes'].remove(attribute)
                    elif mutation == 'substitute':
                        attribute['owner'] = ['other', 'other']
                    elif mutation == 'cfg':
                        attribute['condition'] = 'windows'
                    elif mutation == 'consumer':
                        row['consumers'][0]['consumers'].append('other.rs:mod')
                    else:
                        row['consumers'].pop()
                    self.assertNotEqual(semantic_identities(
                        {r['path']: r for r in reports[1]['files']}), intact_identities)
                    self.assert_refused(
                        'descendant semantic attribute/cfg/consumer identity differs', reports)

    def test_descendant_51_identities_cannot_disappear_or_collide(self):
        # Isolate size identity: removing an attribute-bearing row would fail
        # the earlier semantic guard instead of exercising this contract.
        path = next(item['new'] for item in self.record['sizes']
                    if not self.record['after'][item['new']]['attributes'])
        for mutation in ('missing', 'substitution', 'collision', 'zero'):
            with self.subTest(mutation=mutation):
                reports = copy.deepcopy(self.reports)
                trees = [dict(tree) for tree in self.trees]
                row = next(r for r in reports[1]['files']
                           if r['path'] == path)
                if mutation == 'missing':
                    reports[1]['files'].remove(row)
                    del trees[1][row['path']]
                elif mutation == 'substitution':
                    old = row['path']
                    row['path'] = 'src/substitution.rs'
                    trees[1][row['path']] = trees[1].pop(old)
                elif mutation == 'collision':
                    reports[1]['files'].append(copy.deepcopy(row))
                else:
                    row['counts']['production'] = 0
                reason = ('descendant source population missing or colliding'
                          if mutation == 'collision' else
                          'descendant transported size identity missing or substituted')
                self.assert_refused(reason, reports, trees)

    def test_descendant_growth_is_measured_from_activation_without_relocation_credit(self):
        # This admitted file shrank at activation. Growing back by one line still fails.
        item = next(item for item in self.record['sizes']
                    if item['counts_after']['production'] >= 600 and
                    item['counts_before']['production'] > item['counts_after']['production'])
        reports = copy.deepcopy(self.reports)
        row = next(row for row in reports[1]['files'] if row['path'] == item['new'])
        row['counts']['production'] += 1
        row['physical_lines'] += 1
        self.assertLessEqual(row['counts']['production'], item['counts_before']['production'])
        self.assert_refused('descendant existing oversize growth: ' + item['new'], reports)

    def test_descendant_new_599_passes_and_600_refuses_without_global_source_freeze(self):
        for count in (599, 600):
            with self.subTest(count=count):
                reports = copy.deepcopy(self.reports)
                trees = [dict(tree) for tree in self.trees]
                path = 'crates/rmux-client/src/new.rs'
                data = b'fn new_function() {}\n' * count
                trees[1][path] = dict(blob='new', data=data)
                reports[1]['files'].append(dict(path=path, blob='new', counts={'production': count},
                    physical_lines=count, semantic_attributes=[], consumers=[]))
                if count == 599:
                    self.assertEqual(len(self.transport(reports, trees)[1]), 51)
                else:
                    self.assert_refused(
                        'descendant new production file reaches 600: ' + path, reports, trees)

    def test_descendant_equal_size_source_change_and_shrink_preserve_transport(self):
        for change in ('source', 'shrink'):
            with self.subTest(change=change):
                reports = copy.deepcopy(self.reports)
                trees = [dict(tree) for tree in self.trees]
                item = self.record['sizes'][0]
                path = item['new']
                original = trees[1][path]
                trees[1][path] = dict(original, data=b'// ordinary source change\n' + original['data'][1:])
                if change == 'shrink':
                    row = next(row for row in reports[1]['files'] if row['path'] == path)
                    row['counts']['production'] -= 1
                    row['physical_lines'] -= 1
                self.assertEqual(len(self.transport(reports, trees)[1]), 51)

    def test_descendant_to_descendant_keeps_normal_comparison_and_checks_semantics(self):
        reports = copy.deepcopy(self.reports)
        reports[0] = copy.deepcopy(reports[1])
        trees = [self.trees[1], self.trees[1]]
        self.assertEqual(self.transport(reports, trees), ({}, {}))
        row = next(row for row in reports[1]['files']
                   if any(a['kind'] == 'allow_dead_code' for a in row['semantic_attributes']))
        attribute = next(a for a in row['semantic_attributes'] if a['kind'] == 'allow_dead_code')
        row['semantic_attributes'].remove(attribute)
        with self.assertRaisesRegex(ValueError,
                '^descendant semantic attribute/cfg/consumer identity differs$'):
            self.transport(reports, trees)


    def test_real_scanned_neutral_descendant_from_historical_and_active_sources(self):
        import json
        from compare import compare, semantic_population
        from inventory import scan
        from test_core_ratchets import BASELINE, BOUNDS, CORE
        audit = self.repo / '.rmux-audit/s2139/scanned-canary'
        audit.mkdir(parents=True, exist_ok=True)
        # Keep the positive historical canary and the current activated epoch.
        for parent in (self.record['activation'], self.revision):
            with self.subTest(parent=parent), tempfile.TemporaryDirectory(dir=audit) as directory:
                fixture = Path(directory)
                subprocess.run(['git', 'init', '-q', str(fixture)], check=True)
                common = git(self.repo, 'rev-parse', '--git-common-dir').decode().strip()
                objects = (self.repo / common / 'objects').resolve()
                borrow_objects(fixture / '.git', objects)
                git(fixture, 'read-tree', parent)
                path = 'crates/rmux-client/src/lib.rs'
                data = git(self.repo, 'show', parent + ':' + path) + b'// neutral descendant comment\n'
                blob = subprocess.check_output(['git', '-C', str(fixture), 'hash-object',
                                                 '-w', '--stdin'], input=data).decode().strip()
                git(fixture, 'update-index', '--cacheinfo', '100644,' + blob + ',' + path)
                tree = git(fixture, 'write-tree').decode().strip()
                env = dict(os.environ, GIT_AUTHOR_NAME='Sidney Sissaoui',
                           GIT_AUTHOR_EMAIL='shideneyu@gmail.com', GIT_COMMITTER_NAME='Sidney Sissaoui',
                           GIT_COMMITTER_EMAIL='shideneyu@gmail.com')
                descendant = subprocess.check_output(['git', '-C', str(fixture), 'commit-tree',
                    tree, '-p', parent, '-m', 'scanned neutral client comment canary'], env=env).decode().strip()
                # Authenticate proof history at the candidate; scan the child by SHA.
                git(fixture, 'update-ref', 'HEAD', self.revision)
                raw = (self.repo / 'scripts/source-inventory/declarations.json').read_bytes()
                from relocation_chain import epoch_declarations
                historical = {
                    self.record['base']: epoch_declarations(fixture, self.record['base'],
                        epochs=('D570-413e4026', 'D570-413e4026-declaration-supplement')),
                    self.record['activation']: epoch_declarations(fixture, self.record['activation'],
                        epochs=('D570-330141c0', 'D570-330141c0-declaration-supplement')),
                }
                historical[descendant] = historical.get(parent, raw)
                reports = [scan(fixture, revision, Path(os.environ['SOURCE_SYNTAX']).resolve(),
                                json.loads(historical.get(revision, raw)), historical.get(revision, raw))
                           for revision in (BASELINE, self.record['base'], self.record['activation'], descendant)]
                for report in reports:
                    self.assertEqual(report['errors'], [])
                current = {row['path']: row for row in reports[-1]['files']}
                for path, bound in BOUNDS.items():
                    address = self.record['core_after'].get(path, path)
                    self.assertGreater(current[address]['counts'].get('production', 0), 0)
                    if path in CORE:
                        self.assertLessEqual(current[address]['counts']['production'], bound)
                if parent != self.record['activation']:
                    # D573(3) option (i): the sealed 4A/3R plus three documented test
                    # consumers are activated, whole, for descendants of its revision.
                    from relocation_chain import admitted_semantics, proved_semantics
                    parent_report = scan(fixture, parent, Path(os.environ['SOURCE_SYNTAX']).resolve(),
                                         json.loads(raw), raw)
                    self.assertEqual(parent_report['errors'], [])
                    actual = semantic_population(reports[-1])[0]
                    self.assertEqual(actual, semantic_population(parent_report)[0])
                    proved = proved_semantics(self.record)
                    activated = admitted_semantics(fixture, descendant, self.record)
                    self.assertEqual(proved, semantic_population(reports[2])[0])
                    self.assertEqual(actual, activated)
                    self.assertNotEqual(actual, proved)
                    delta = compare(fixture, parent_report, reports[-1], epoch=EPOCH)
                    for key in ('semantic_attribute_additions', 'semantic_attribute_removals',
                                'production_size_violations', 'literal_ratchet_increases'):
                        self.assertEqual(delta[key], [], key)
                    for before in reports[:-1]:
                        with self.subTest(before=before['revision']):
                            compare(fixture, before, reports[-1], epoch=EPOCH)
                    continue
                baseline_delta = compare(fixture, reports[0], reports[-1], epoch=EPOCH)
                self.assertEqual([row for row in baseline_delta['semantic_attribute_additions']
                                  if row['path'] == 'crates/rmux-core/src/session/layout_cycle.rs'], [])
                for before in reports[1:3]:
                    delta = compare(fixture, before, reports[-1], epoch=EPOCH)
                    for key in ('semantic_attribute_additions', 'semantic_attribute_removals',
                                'production_size_violations', 'literal_ratchet_increases'):
                        self.assertEqual(delta[key], [], key)
                    self.assertEqual(delta['credited_reduction'], 0)
