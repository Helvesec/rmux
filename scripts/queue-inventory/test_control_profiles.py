"""Frozen acceptance for R2-246 F1/F2 and W05; source contract, no runtime change."""
import contextlib
import copy
import io
import json
import os
from pathlib import Path
import unittest
from unittest.mock import patch

from run_references import execute


ROOT = Path(__file__).resolve().parents[2]
CONTROL_TESTS = 'crates/rmux-server/src/control/tests.rs'
UNIX_SYMBOLS = {
    'control_input_rejects_excessive_queued_lines',
    'control_input_rejects_unterminated_oversize_lines',
    'shutdown_cancels_only_the_explicit_control_wait',
    'output_queue_tracks_buffered_bytes',
    'dropping_active_control_command_aborts_inflight_task',
    'live_kill_server_stops_buffered_frames_before_shutdown_watch_propagates',
}
SUBSCRIPTIONS = 'crates/rmux-server/src/control/subscriptions.rs'
DELIVERY = 'crates/rmux-server/src/control/pane_event_delivery.rs'


def simulated_host(platform):
    # These fixtures list and execute images directly; an ambient identity
    # runner from a gate would add a host-specific `check` they do not model.
    env = {k: v for k, v in os.environ.items() if not k.startswith('RMUX_IDENTITY_')}
    stack = contextlib.ExitStack()
    stack.enter_context(patch.dict(os.environ, env, clear=True))
    stack.enter_context(patch('run_references.host_platform', return_value=platform))
    return stack


class ControlProfileAcceptance(unittest.TestCase):
    def setUp(self):
        self.profiles = json.loads((ROOT / 'scripts/queue-inventory/profiles.json').read_text())

    def test_all_five_unix_references_remain_and_exclude_windows(self):
        self.assertIn('#[cfg(all(test, unix))]\n#[path = "control/tests.rs"]',
                      (ROOT / 'crates/rmux-server/src/control.rs').read_text())
        references = [test for profile in self.profiles.values()
                      for test in profile['tests'] if test['path'] == CONTROL_TESTS]
        self.assertEqual({test['symbol'] for test in references}, UNIX_SYMBOLS)
        self.assertEqual(len(references), 18, 'no reference may be removed')
        for test in references:
            with self.subTest(symbol=test['symbol']):
                self.assertEqual(test['platforms'], ['linux', 'macos'])

    def test_pending_budget_ends_before_unbounded_output_enqueue(self):
        source = (ROOT / SUBSCRIPTIONS).read_text()
        self.assertLess(source.index('PaneEventRetention::into_output_age'),
                        source.index('output_queue.enqueue_pane_output'))
        self.assertIn('drop(output.admission.take());', (ROOT / DELIVERY).read_text())
        for key in ('channel:server/control.rs:376', 'control-pending-pane-events'):
            profile = self.profiles[key]
            with self.subTest(profile=key):
                self.assertIn('before enqueue', profile['consumer'])
                self.assertIn('only age until write/discard', profile['closure'])
                self.assertIn('ControlOutputQueue is unbounded', profile['closure'])
                self.assertNotIn('keeps permits until write/discard', profile['closure'])
                anchors = {(a['path'], a['snippet']) for a in profile['evidence']}
                self.assertIn((SUBSCRIPTIONS, 'PaneEventRetention::into_output_age'), anchors)
                self.assertIn((DELIVERY, 'drop(output.admission.take());'), anchors)
        rows = json.loads((ROOT / 'scripts/queue-inventory/sites.json').read_text())
        output = [r for r in rows if r['site']['path'].endswith('/control/output_queue.rs')]
        self.assertTrue(any(r.get('category') == 'unbounded' for r in output))

    def test_real_references_select_only_compiled_control_tests_on_each_platform(self):
        references = [test for profile in self.profiles.values()
                      for test in profile['tests'] if test['path'] == CONTROL_TESTS]
        sentinel = {'path': 'crates/rmux-server/src/control.rs', 'package': 'rmux-server',
                    'target': 'lib', 'symbol': 'deferred_control_notifications_are_bounded',
                    'platforms': ['linux', 'macos', 'windows']}
        for platform in ('linux', 'macos', 'windows'):
            executed = []

            def native_fixture(command, root, timeout=None):
                if command[0] == 'cargo':
                    return json.dumps({'reason': 'compiler-artifact', 'executable': '/fixture/test',
                                       'profile': {'test': True}})
                if '--list' in command:
                    # This listing comes from control.rs's cfg, never profile platforms.
                    symbols = {sentinel['symbol']} | (UNIX_SYMBOLS if platform != 'windows' else set())
                    return ''.join(f'control::tests::{name}: test\n' for name in sorted(symbols))
                executed.append(command[command.index('--exact') - 1].split('::')[-1])
                return 'test result: ok. 1 passed; 0 failed; 0 ignored;\n'

            with self.subTest(platform=platform), simulated_host(platform), \
                    patch('run_references.run', native_fixture):
                report = execute(ROOT, {'control': {'tests': references + [sentinel]}})
                expected = {sentinel['symbol']} | (UNIX_SYMBOLS if platform != 'windows' else set())
                self.assertEqual(set(executed), expected)
                self.assertEqual(report['passed'], len(expected))
                self.assertEqual(report['other_platform_references'], 6 if platform == 'windows' else 0)


# Frozen Windows listing (W05): every rmux-server lib test name, from the native
# INT1135 run at c570d96542dd (Windows host, x86_64-pc-windows-msvc), whose last path
# segment is a profile symbol. Source: evidence/windows/09-workspace.log lines
# 6399-11838 (sha256 691716701e06438849c671d016818a068e7ffa093decd65fd207a636dc42899d),
# 5152 names. This is a recorded listing, not a native run.
WINDOWS_SERVER_LIB_LISTING = (
    'control::deferred_tests::deferred_control_notification_bytes_are_bounded_and_accounted_on_pop',
    'control::deferred_tests::deferred_control_notifications_are_bounded',
    'control::format_subscriptions::tests::definition_batches_are_bounded_before_registry_mutation',
    'control::format_subscriptions::tests::notification_batches_are_bounded_by_count_and_encoded_bytes',
    'control::format_subscriptions::tests::registry_count_and_bytes_are_bounded_transactionally',
    'control::format_subscriptions::wildcard_tests::retained_values_are_bounded_without_storing_overflow_baseline',
    'control::format_subscriptions::wildcard_tests::wildcard_target_cache_is_bounded_without_storing_overflow_target',
    'control::subscriptions::fifo_tests::control_cursor_gap_keeps_slow_client_and_fast_peer_alive',
    'control::subscriptions::fifo_tests::fifo_budget_barrier_fences_later_live_output',
    'control::subscriptions::fifo_tests::fifo_budget_barrier_includes_unscheduled_initial_snapshots',
    'control::subscriptions::fifo_tests::fifo_budget_lagged_event_retains_until_consumption',
    'control::subscriptions::fifo_tests::fifo_budget_multipane_snapshots_stay_below_four_mib',
    'control::subscriptions::fifo_tests::fifo_budget_ready_delivery_allows_a_waiting_peer_to_run',
    'control::subscriptions::fifo_tests::fifo_budget_saturated_channel_retains_until_consumption',
    'handler::alert_tests::show_messages_formats_log_and_terminal_info_and_prunes_to_limit',
    'handler::alert_tests::show_messages_invalid_target_client_returns_error',
    'handler::alert_tests::show_messages_log_is_available_without_current_client',
    'handler::alert_tests::show_messages_message_log_resolves_target_client',
    'handler::attach_support::tests::attach_control_backlog_limit_removes_slow_client',
    'handler::background_tasks::tests::background_shutdown_cancels_a_pending_opt_in_lifecycle_worker',
    'handler::background_tasks::tests::lifecycle_worker_close_drains_an_active_mutation',
    'handler::background_tasks::tests::lifecycle_worker_pending_is_cancelled_and_releases_its_registration',
    'handler::background_tasks::tests::lifecycle_worker_preserves_hook_lane_until_final_close',
    'handler::background_tasks::tests::shutdown_cancels_and_joins_a_started_background_task',
    'handler::clipboard_query_tests::both_mode_stores_response_with_buffer_limit_without_pane_set_clipboard_hook',
    'handler::clipboard_query_tests::pane_alert_clipboard_query_queue_is_bounded_while_the_worker_is_busy',
    'handler::clipboard_query_tests::pending_queries_are_fifo_bounded_and_expire',
    'handler::lifecycle_dispatch_queue::tests::active_outbox_backpressures_without_dropping_or_reordering',
    'handler::lifecycle_producer_tasks::tests::close_cancels_a_pending_task_before_mutation',
    'handler::lifecycle_producer_tasks::tests::close_cancels_follow_on_pending_work_after_a_mutation',
    'handler::lifecycle_producer_tasks::tests::close_waits_for_mutation_and_publication',
    'handler::lifecycle_producer_tasks::tests::normal_close_keeps_the_lifecycle_hook_lane_open_until_final_close',
    'handler::lifecycle_support::tests::pane_exit_hook_current_target_falls_back_when_window_closed',
    'handler::lifecycle_support::tests::prepared_lifecycle_events_publish_in_commit_order_not_waiter_order',
    'handler::overlay_support::popup_job::tests::dropping_last_popup_io_queue_cancels_worker_and_releases_receipts',
    'handler::overlay_support::popup_job::tests::popup_io_queue_preserves_write_resize_write_enqueue_order',
    'handler::overlay_support::popup_job::tests::popup_io_queue_saturation_cancels_active_and_pending_work',
    'handler::pane_pipe_tests::pipe_pane_once_closes_existing_pipe_without_reopening',
    'handler::post_commit_sequencer::tests::cancelling_waiter_preserves_order_before_releasing_successor',
    'handler::post_commit_sequencer::tests::normal_shutdown_lane_drains_accepted_work_but_keeps_lifecycle_hook_lane_open',
    'handler::post_commit_sequencer::tests::reservations_execute_in_reservation_order',
    'handler::post_commit_sequencer::tests::shutdown_waits_for_an_accepted_action_and_rejects_new_work',
    'handler::prompt_support::history::tests::history_push_deduplicates_consecutive',
    'handler::prompt_support::history::tests::history_respects_limit',
    'handler::prompt_support::history::tests::history_types_are_independent',
    'handler::prompt_support::history::tests::history_up_down_restores_original_buffer',
    'handler::prompt_support::history::tests::history_zero_limit_clears',
    'handler::scripting_tests::control_hooks_wait::wait_for_cancellation_removes_plain_and_lock_waiters',
    'handler::scripting_tests::control_hooks_wait::wait_for_shutdown_releases_plain_and_lock_waiters',
    'handler::scripting_tests::control_hooks_wait::wait_for_unlock_hands_locks_to_queued_waiters_in_fifo_order',
    'handler::scripting_tests::runtime_execution::runtime_execution_shutdown_bounds_resolved_foreground_shells',
    'handler::shutdown_support::tests::idle_shutdown_retries_after_in_flight_detached_request',
    'handler::subscription_support::output_subscription_state::tests::one_surface_initializer_wakes_all_waiters_before_re_election',
    'handler::subscription_support::output_subscription_state::tests::pane_removal_wakes_surface_initialization_waiters',
    'handler::subscription_support::output_subscription_state::tests::surface_initialization_token_survives_a_pane_rekey',
    'handler::tests::overlay_tests::popup_burst::popup_500k_burst_keeps_one_pending_and_one_in_flight_at_225x54',
    'handler::tests::overlay_tests::popup_burst::popup_pending_close_prevents_late_repaint',
    'handler::tests::overlay_tests::popup_burst::popup_pending_flush_preserves_resize_and_final_exit_surface',
    'handler::tests::overlay_tests::popup_burst::popup_pending_flush_rejects_stale_attach_and_replacement',
    'handler::tests::overlay_tests::popup_burst::popup_reader_ticks_coalesce_actual_production_before_flush',
    'handler::tests::send_keys_tests::live_attach::live_attach_prompt_cancel_chain_is_processed_iteratively',
    'handler::wait_support::tests::byte_observer_matches_across_event_boundaries_without_unbounded_tail',
    'handler::wait_support::tests::cancellation_keeps_quota_charged_until_wait_storage_is_gone',
    'handler::wait_support::tests::connection_teardown_cancels_only_that_connections_sdk_waits',
    'handler::wait_support::tests::connection_teardown_does_not_cancel_or_charge_another_client',
    'handler::wait_support::tests::dropping_unpolled_wait_releases_quota_after_prepare_error_path',
    'handler::wait_support::tests::matched_wait_releases_quota_after_its_pattern_is_dropped',
    'handler::wait_support::tests::timed_out_wait_future_releases_quota_via_drop_guard',
    'handler::wait_support::tests::wait_for_bytes_returns_cancelled_when_registry_sends_cancel',
    'handler::web_support::tests::web_session_pane_scroll_frame_degrades_when_recovery_metadata_is_bounded_out',
    'pane_io::attach_transport::timeout_tests::saturated_non_reader_is_bounded_by_the_write_timeout',
    'pane_io::deferred_passthrough::tests::held_passthroughs_stay_bounded_while_the_interval_is_open',
    'pane_io::deferred_passthrough::tests::same_target_keeps_deferred_passthroughs',
    'pane_io::types::tests::live_fast_output_queue_capacity_is_bounded',
    'pane_io::types::tests::live_passthrough_retention_is_bounded',
    'pane_io::types::tests::live_receiver_uses_bounded_ring_for_large_output',
    'pane_recovery::tests::keyframe_keeps_a_bounded_recent_history_suffix_in_alternate_screen',
    'pane_recovery::tests::keyframe_materializes_saved_main_history_after_alternate_height_shrink',
    'pane_recovery::tests::keyframe_never_splits_one_oversized_wrapped_history_group',
    'pane_recovery::tests::keyframe_preserves_complete_continuation_erase_wrap_boundaries',
    'pane_recovery::tests::keyframe_preserves_wide_cells_and_wrap_boundaries',
    'pane_rename_queue::pane_rename_queue_tests::a_prefix_drain_returns_only_the_capacity_it_took',
    'pane_rename_queue::pane_rename_queue_tests::an_admission_that_accepts_nothing_returns_its_capacity',
    'pane_state_journal::tests::capacity_rejects_n_plus_one_and_retains_oldest_closed_until_delivery',
    'pane_state_journal::tests::closed_pane_tracking_is_bounded_by_journal_capacity',
    'pane_state_journal::tests::closed_subscription_is_bounded_at_close_revision_after_reopen',
    'pane_state_journal::tests::journal_byte_budget_evicts_oversized_records_and_preserves_cursor_progress',
    'pane_terminals::deferred_initial::tests::queued_bracketed_paste_is_charged_against_the_starting_input_limit',
    'status_jobs::tests::status_job_runtime_bounds_active_workers',
    'web::outbound::tests::data_slot_accounts_backlog_before_publication',
    'web::outbound::tests::dropping_unsent_data_slot_rolls_back_accounting',
    'web::outbound::tests::in_flight_keyframe_remains_in_the_shared_backlog_budget',
    'web::outbound::tests::keyframe_replaces_latest_even_when_data_queue_is_full',
    'web::outbound::tests::keyframe_then_data_respects_shared_backlog_limit',
    'web::outbound::tests::replacing_latest_keyframe_transfers_its_backlog_reservation',
    'web::outbound::tests::write_ping_enqueues_control_ping_with_payload',
    'web::server::pre_auth::tests::pre_auth_queue_enforces_per_ip_capacity',
    'web::server::pre_auth::tests::pre_auth_queue_skips_protected_entries_when_shedding_load',
    'web::server::pre_auth::tests::pre_auth_replacement_waits_for_the_cancelled_slot_to_release',
    'web::server::streams::tests::session_refresh_deadline_is_bounded_by_max_wait',
    'web::server::tests::shutdown_closes_established_pane_and_session_websockets_and_forwarder',
    'web::stream_sanitizer::tests::allowed_visual_osc_survives_all_fragmentation_boundaries',
    'web::stream_sanitizer::tests::osc_52_is_removed_at_every_fragmentation_boundary',
    'web::stream_sanitizer::tests::oversized_osc_8_closes_a_prior_hyperlink_for_bel_st_and_both_roles',
    'web::stream_sanitizer::tests::oversized_osc_discard_ends_at_bell',
    'web::stream_sanitizer::tests::rejected_osc_8_closes_a_prior_hyperlink_at_every_fragmentation_boundary',
    'web::tests::stop_all_reports_removed_share_count',
    'web::tests::stopped_or_expired_share_rejects_previous_tokens',
    'web::tunnel::output::tests::reader_bounds_overlong_lines_and_resumes',
    'web::tunnel::output::tests::reader_forwards_final_unterminated_line',
)
FIFO_SYMBOLS = {
    'control_cursor_gap_keeps_slow_client_and_fast_peer_alive',
    'fifo_budget_barrier_fences_later_live_output',
    'fifo_budget_barrier_includes_unscheduled_initial_snapshots',
    'fifo_budget_lagged_event_retains_until_consumption',
    'fifo_budget_multipane_snapshots_stay_below_four_mib',
    'fifo_budget_ready_delivery_allows_a_waiting_peer_to_run',
    'fifo_budget_saturated_channel_retains_until_consumption',
}
ECHO_SYMBOLS = {
    'predicted_local_echo_accepts_only_single_printable_bytes',
    'predicted_local_echo_accepts_printable_prefix_before_enter',
    'predicted_local_echo_consumes_exact_pty_echo_once',
    'stale_predicted_local_echo_expires_without_pty_echo',
}


def server_lib_profiles(profiles):
    return {key: {'tests': [test for test in profile['tests']
                            if (test['package'], test['target']) == ('rmux-server', 'lib')]}
            for key, profile in profiles.items()}


def run_windows_listing(profiles):
    executed = []

    def native_fixture(command, root, timeout=None):
        if command[0] == 'cargo':
            return json.dumps({'reason': 'compiler-artifact', 'executable': '/fixture/test',
                               'profile': {'test': True}})
        if '--list' in command:
            return ''.join(f'{name}: test\n' for name in WINDOWS_SERVER_LIB_LISTING)
        executed.append(command[command.index('--exact') - 1])
        return 'test result: ok. 1 passed; 0 failed; 0 ignored;\n'

    with simulated_host('windows'), \
            patch('run_references.run', native_fixture), contextlib.redirect_stdout(io.StringIO()):
        return execute(ROOT, server_lib_profiles(profiles)), executed


class WindowsListingProfileAcceptance(unittest.TestCase):
    def setUp(self):
        self.profiles = json.loads((ROOT / 'scripts/queue-inventory/profiles.json').read_text())

    def platforms(self, symbols):
        found = {test['symbol']: test['platforms'] for profile in self.profiles.values()
                 for test in profile['tests'] if test['symbol'] in symbols}
        self.assertEqual(set(found), symbols, 'no reference may be removed')
        return found

    def test_profiles_follow_the_compiled_cfg(self):
        self.assertIn('#[cfg(test)]\nmod fifo_tests;', (ROOT / SUBSCRIPTIONS).read_text())
        self.assertIn('#[cfg(all(test, unix))]\nmod tests;',
                      (ROOT / 'crates/rmux-server/src/pane_io.rs').read_text())
        for symbol, platforms in self.platforms(FIFO_SYMBOLS).items():
            with self.subTest(symbol=symbol):
                self.assertEqual(platforms, ['linux', 'macos', 'windows'])
        for symbol, platforms in self.platforms(ECHO_SYMBOLS).items():
            with self.subTest(symbol=symbol):
                self.assertEqual(platforms, ['linux', 'macos'])

    def test_real_references_accept_the_frozen_windows_listing(self):
        report, executed = run_windows_listing(self.profiles)
        declared = {test['symbol'] for profile in server_lib_profiles(self.profiles).values()
                    for test in profile['tests'] if 'windows' in test['platforms']}
        self.assertEqual({name.split('::')[-1] for name in executed}, declared)
        self.assertTrue(FIFO_SYMBOLS <= declared and not ECHO_SYMBOLS & declared)
        self.assertEqual(report['passed'], len(executed))

    def test_removing_either_correction_is_refused_by_the_real_references(self):
        for symbols, add, message in (
                (FIFO_SYMBOLS, False, 'incorrectly excluded native test'),
                (ECHO_SYMBOLS, True, 'expected one native test')):
            mutated = copy.deepcopy(self.profiles)
            for profile in mutated.values():
                for test in profile['tests']:
                    if test['symbol'] in symbols:
                        test['platforms'] = ['linux', 'macos'] + (['windows'] if add else [])
            with self.subTest(message=message):
                with self.assertRaises(ValueError) as refused:
                    run_windows_listing(mutated)
                errors = str(refused.exception).splitlines()
                self.assertEqual(len(errors), len(symbols))
                for symbol in symbols:
                    self.assertTrue(any(message in e and symbol in e for e in errors), symbol)


if __name__ == '__main__':
    unittest.main()
