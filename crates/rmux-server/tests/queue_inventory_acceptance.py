"""Acceptance shared by the native Cargo target and Python-only checks."""
import argparse
import json
import subprocess
import sys


def pane_alert_store_acceptance(root):
    """Bind the coalescer's store and exemption to its current Rust source."""
    sys.path.insert(0, str(root / "scripts/queue-inventory"))
    from check_inventory import current_inventory
    from validation import validate_inventory

    path = "crates/rmux-server/src/handler/alert_support/pane_alert_coalescer.rs"
    sources = {path: (root / path).read_text(encoding="utf-8")}
    registry = root / "scripts/queue-inventory"
    stores = [store for store in json.loads((registry / "stores.json").read_text())
              if store["name"] == "pane-alert-coalescer"]
    assert len(stores) == 1, "the coalescer must have exactly one store locator"
    rows = [row for row in json.loads((registry / "sites.json").read_text())
            if row["site"]["expression"] == stores[0]["anchor"]]
    assert len(rows) == 1, "the coalescer must retain its inventory row"
    inventory, sites = current_inventory(sources, {}, rows, stores)
    validate_inventory(inventory, sites, sources)


def prompt_history_store_acceptance(root):
    """Bind prompt history's locator, capacity evidence and tests to its source."""
    sys.path.insert(0, str(root / "scripts/queue-inventory"))
    from check_inventory import current_inventory
    from validation import validate_inventory

    path = "crates/rmux-server/src/handler/prompt_support/history.rs"
    sources = {path: (root / path).read_text(encoding="utf-8")}
    registry = root / "scripts/queue-inventory"
    stores = [store for store in json.loads((registry / "stores.json").read_text())
              if store["name"] == "prompt-history"]
    assert len(stores) == 1, "prompt history must have exactly one store locator"
    rows = [row for row in json.loads((registry / "sites.json").read_text())
            if row.get("profile") == "prompt-history"]
    assert len(rows) == 1, "prompt history must retain its inventory row"
    profiles = json.loads((registry / "profiles.json").read_text())
    inventory, sites = current_inventory(
        sources, {"prompt-history": profiles["prompt-history"]}, rows, stores)
    validate_inventory(inventory, sites, sources)


def attach_unix_channels_acceptance(root):
    """Keep all three moved attach channels bound to their current source."""
    sys.path.insert(0, str(root / "scripts/queue-inventory"))
    from check_inventory import current_inventory
    from validation import validate_inventory

    path = "crates/rmux-client/src/attach/unix.rs"
    old_path = "crates/rmux-client/src/attach.rs"
    sources = {path: (root / path).read_text(encoding="utf-8")}
    registry = root / "scripts/queue-inventory"
    rows = [row for row in json.loads((registry / "sites.json").read_text())
            if row["site"]["path"] in (old_path, path)]
    assert len(rows) == 3, "attach must retain all three channel locators"
    inventory, sites = current_inventory(sources, {}, rows, [])
    validate_inventory(inventory, sites, sources)


def attach_windows_queues_acceptance(root):
    """Bind the moved Windows attach family, keeping its retained test source."""
    sys.path.insert(0, str(root / "scripts/queue-inventory"))
    from check_inventory import current_inventory
    from validation import validate_inventory

    paths = {
        "crates/rmux-client/src/attach_windows.rs":
            "crates/rmux-client/src/attach/windows.rs",
        "crates/rmux-client/src/attach_windows/output_worker.rs":
            "crates/rmux-client/src/attach/windows/output_worker.rs",
        "crates/rmux-client/src/attach_windows/stream.rs":
            "crates/rmux-client/src/attach/windows/stream.rs",
        "crates/rmux-client/src/attach_windows/stream/exclusive_actions.rs":
            "crates/rmux-client/src/attach/windows/stream/exclusive_actions.rs",
        "crates/rmux-client/src/attach/windows/stream/output_queue.rs":
            "crates/rmux-client/src/attach/windows/stream/output_queue.rs",
        "crates/rmux-client/src/attach/windows/stream/spool.rs":
            "crates/rmux-client/src/attach/windows/stream/spool.rs",
    }
    source_paths = [*paths.values(), "crates/rmux-client/src/attach_windows/stream_tests.rs"]
    sources = {path: (root / path).read_text(encoding="utf-8") for path in source_paths}
    registry = root / "scripts/queue-inventory"
    rows = [row for row in json.loads((registry / "sites.json").read_text())
            if row["site"]["path"] in paths or row["site"]["path"] in paths.values()]
    assert len(rows) == 21, "Windows attach must retain all 21 queue locators"
    profile_ids = {row["profile"] for row in rows if "profile" in row}
    profiles = json.loads((registry / "profiles.json").read_text())
    inventory, sites = current_inventory(
        sources, {key: profiles[key] for key in profile_ids}, rows, [])
    validate_inventory(inventory, sites, sources)


def ipc_drain_channel_acceptance(root):
    """Bind the moved drain channel, its capacity evidence and all four tests."""
    sys.path.insert(0, str(root / "scripts/queue-inventory"))
    from check_inventory import current_inventory
    from validation import validate_inventory

    path = "crates/rmux-ipc/src/server_write_drain/operation.rs"
    old_path = "crates/rmux-ipc/src/server_write_drain_operation.rs"
    profile_id = "signal:ipc/server_write_drain_operation.rs:76"
    sources = {path: (root / path).read_text(encoding="utf-8")}
    registry = root / "scripts/queue-inventory"
    rows = [row for row in json.loads((registry / "sites.json").read_text())
            if row["site"]["path"] in (old_path, path)
            or row.get("profile") == profile_id]
    assert len(rows) == 1, "the drain must retain exactly one channel locator"
    profiles = json.loads((registry / "profiles.json").read_text())
    profile = profiles[profile_id]
    assert len(profile["tests"]) == 4, "the drain must retain all four test references"
    inventory, sites = current_inventory(sources, {profile_id: profile}, rows, [])
    validate_inventory(inventory, sites, sources)


def output_subscription_channels_acceptance(root):
    """Bind both initialization channels and their evidence/tests after the move."""
    sys.path.insert(0, str(root / "scripts/queue-inventory"))
    from check_inventory import current_inventory
    from validation import validate_inventory

    path = "crates/rmux-server/src/handler/subscription_support/output_subscription_state.rs"
    old_path = "crates/rmux-server/src/handler/output_subscription_state.rs"
    profile_ids = (
        "signal:server/handler/output_subscription_state.rs:375",
        "signal:server/handler/output_subscription_state.rs:398",
    )
    sources = {path: (root / path).read_text(encoding="utf-8")}
    registry = root / "scripts/queue-inventory"
    rows = [row for row in json.loads((registry / "sites.json").read_text())
            if row["site"]["path"] in (old_path, path)
            or row.get("profile") in profile_ids]
    assert len(rows) == 2, "output subscription must retain both channel locators"
    profiles = json.loads((registry / "profiles.json").read_text())
    profiles = {key: profiles[key] for key in profile_ids}
    for profile in profiles.values():
        assert len(profile["tests"]) == 3, "each channel must retain all three test references"
    inventory, sites = current_inventory(sources, profiles, rows, [])
    validate_inventory(inventory, sites, sources)


def silence_timer_worklist_acceptance(root):
    """Bind the moved timer declaration, worklist and evidence to their source."""
    sys.path.insert(0, str(root / "scripts/queue-inventory"))
    from check_inventory import current_inventory
    from validation import validate_inventory

    path = "crates/rmux-server/src/handler/alert_support/silence_timers/lifecycle.rs"
    old_path = "crates/rmux-server/src/handler_alerts/silence_timers/lifecycle.rs"
    sources = {path: (root / path).read_text(encoding="utf-8")}
    registry = root / "scripts/queue-inventory"
    rows = [row for row in json.loads((registry / "sites.json").read_text())
            if row["site"]["path"] in (old_path, path)]
    assert len(rows) == 2, "silence timers must retain both declaration and worklist locators"
    inventory, sites = current_inventory(sources, {}, rows, [])
    validate_inventory(inventory, sites, sources)


def attached_input_fifo_acceptance(root):
    """Bind the moved input FIFO declaration and its evidence to current source."""
    sys.path.insert(0, str(root / "scripts/queue-inventory"))
    from check_inventory import current_inventory
    from validation import validate_inventory

    path = "crates/rmux-server/src/handler/attach_support/input_path.rs"
    old_path = "crates/rmux-server/src/handler_attach/input_path.rs"
    sources = {path: (root / path).read_text(encoding="utf-8")}
    registry = root / "scripts/queue-inventory"
    rows = [row for row in json.loads((registry / "sites.json").read_text())
            if row["site"]["path"] in (old_path, path)]
    assert len(rows) == 1, "attached input must retain exactly one FIFO locator"
    inventory, sites = current_inventory(sources, {}, rows, [])
    validate_inventory(inventory, sites, sources)


def background_shutdown_channel_acceptance(root):
    """Bind the moved shutdown channel while retaining its original test source."""
    sys.path.insert(0, str(root / "scripts/queue-inventory"))
    from check_inventory import current_inventory
    from validation import validate_inventory

    path = "crates/rmux-server/src/handler/background_tasks.rs"
    old_path = "crates/rmux-server/src/handler_background_tasks.rs"
    test_path = "crates/rmux-server/src/handler_background_tasks/tests.rs"
    profile_id = "signal:server/handler_background_tasks.rs:97"
    sources = {p: (root / p).read_text(encoding="utf-8") for p in (path, test_path)}
    registry = root / "scripts/queue-inventory"
    rows = [row for row in json.loads((registry / "sites.json").read_text())
            if row["site"]["path"] in (old_path, path)
            or row.get("profile") == profile_id]
    assert len(rows) == 1, "background shutdown must retain exactly one channel locator"
    profiles = json.loads((registry / "profiles.json").read_text())
    profile = profiles[profile_id]
    assert len(profile["tests"]) == 5, "background shutdown must retain all five test references"
    inventory, sites = current_inventory(sources, {profile_id: profile}, rows, [])
    validate_inventory(inventory, sites, sources)


def client_attach_channel_acceptance(root):
    """Bind client attach's moved channel and its shared capacity/test evidence."""
    sys.path.insert(0, str(root / "scripts/queue-inventory"))
    from check_inventory import current_inventory
    from validation import validate_inventory

    path = "crates/rmux-server/src/handler/client_support/attach.rs"
    old_path = "crates/rmux-server/src/handler_client/attach.rs"
    source_paths = (
        path,
        "crates/rmux-server/src/handler/attach_support.rs",
        "crates/rmux-server/src/pane_io/attach_control.rs",
    )
    sources = {p: (root / p).read_text(encoding="utf-8") for p in source_paths}
    registry = root / "scripts/queue-inventory"
    rows = [row for row in json.loads((registry / "sites.json").read_text())
            if row["site"]["path"] in (old_path, path)]
    assert len(rows) == 1, "client attach must retain exactly one channel locator"
    assert rows[0].get("profile") == "attach-control", "retain the shared control profile"
    profiles = json.loads((registry / "profiles.json").read_text())
    profile = profiles["attach-control"]
    assert len(profile["tests"]) == 1, "attach control must retain its backlog test"
    inventory, sites = current_inventory(sources, {"attach-control": profile}, rows, [])
    validate_inventory(inventory, sites, sources)


def clipboard_query_queues_acceptance(root):
    """Bind both moved clipboard queues while retaining their original test source."""
    sys.path.insert(0, str(root / "scripts/queue-inventory"))
    from check_inventory import current_inventory
    from validation import validate_inventory

    path = "crates/rmux-server/src/handler/clipboard_query_support.rs"
    old_path = "crates/rmux-server/src/handler_clipboard_query.rs"
    test_path = "crates/rmux-server/src/handler_clipboard_query_tests.rs"
    profile_ids = ("clipboard-pending", "clipboard-queued")
    sources = {p: (root / p).read_text(encoding="utf-8") for p in (path, test_path)}
    registry = root / "scripts/queue-inventory"
    rows = [row for row in json.loads((registry / "sites.json").read_text())
            if row["site"]["path"] in (old_path, path)
            or row.get("profile") in profile_ids]
    assert len(rows) == 2, "clipboard queries must retain both queue locators"
    assert {row.get("profile") for row in rows} == set(profile_ids)
    profiles = json.loads((registry / "profiles.json").read_text())
    profiles = {key: profiles[key] for key in profile_ids}
    for key, count in zip(profile_ids, (3, 2)):
        assert len(profiles[key]["tests"]) == count, "retain all clipboard test references"
    inventory, sites = current_inventory(sources, profiles, rows, [])
    validate_inventory(inventory, sites, sources)


def acceptance(root):
    pane_alert_store_acceptance(root)
    prompt_history_store_acceptance(root)
    attach_unix_channels_acceptance(root)
    attach_windows_queues_acceptance(root)
    ipc_drain_channel_acceptance(root)
    output_subscription_channels_acceptance(root)
    silence_timer_worklist_acceptance(root)
    attached_input_fifo_acceptance(root)
    background_shutdown_channel_acceptance(root)
    client_attach_channel_acceptance(root)
    clipboard_query_queues_acceptance(root)
    checker = root / "scripts/queue-inventory/check_inventory.py"
    assert checker.is_file(), "the queue registry must be checked against current product by the gates"
    output = subprocess.run(
        [sys.executable, "-B", str(checker), "--source", str(root), "--self-test"],
        capture_output=True, text=True, check=False,
    )
    assert output.returncode == 0, output.stdout + output.stderr
    report = json.loads(output.stdout)
    assert report["verdict"] == "PASS", report
    assert report["bounded_entries"] >= 105, "the existing 105 queue sites must remain covered"
    assert report["negative_controls"] == [
        "missing entry", "uncovered production queue", "absent referenced test",
        "false primitive capacity", "false qualified primitive capacity",
    ], "missing absence or false-bound canary"
    print(output.stdout, end="")


if __name__ == "__main__":
    from pathlib import Path
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", required=True, type=Path)
    acceptance(parser.parse_args().source.resolve())
