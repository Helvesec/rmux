"""Exact job and artifact topology for fail-closed receipt recovery."""

ACTIVE = frozenset({"queued", "in_progress", "requested", "waiting", "pending"})
DIRECT_SUCCESS_JOBS = frozenset(
    {
        "Verify immutable Release and create receipt",
        "Audit live downstream repository authority",
    }
)
PREPARATION_SUCCESS_JOBS = frozenset(
    {
        "Prepare non-authoritative downstream plan",
        "Verify exact downstream repository authority audit",
        "Prepare exact downstream payloads / Materialize exact channel payloads",
    }
)
EARLY_SUCCESS_JOBS = DIRECT_SUCCESS_JOBS | PREPARATION_SUCCESS_JOBS
LINUX_SIGNING_JOB = "Build exact signed Linux repository trees / Sign retained APT and RPM repository trees"
OWNED_WRITER_JOBS = frozenset(
    {
        "Publish exact Homebrew tap formula / Publish owned channel homebrew_tap",
        "Publish exact Scoop manifest / Publish owned channel scoop",
        "Publish exact Web Share WASM bytes / Publish owned channel web_share",
    }
)
SKIPPED_AFTER_FAILURE = frozenset(
    {
        "Build exact signed Linux repository trees / Publish only this run's authorized recovery artifact",
        "Publish exact signed APT and RPM repositories",
        "Publish exact crates.io package set",
        "Record denied RC Linux repository channel",
        "Submit exact Chocolatey package",
        "Record disabled Snap stable channel",
        "Record manual Homebrew Core submission",
        "Record manual WinGet submission",
        "Publish exact Snap candidate revisions",
        "Aggregate ten exact pre-site results",
        "Record blocked automated rmux.io update",
        "Prepare manual rmux.io handoff",
        "Aggregate all eleven exact channel results",
    }
)
POST_MUTATION_SKIPPED_AFTER_FAILURE = SKIPPED_AFTER_FAILURE - {
    "Build exact signed Linux repository trees / Publish only this run's authorized recovery artifact"
}
LEGACY_DOWNSTREAM_PREFIX = "Receipt-gated downstream publication / "
PREPARATION_PREFIX = "Prepare receipt-gated downstream publication / "
PAYLOAD_CHANNELS = (
    "apt_rpm",
    "chocolatey",
    "crates_io",
    "homebrew_core",
    "homebrew_tap",
    "scoop",
    "snap_candidate",
    "snap_stable",
    "web_share",
    "winget",
)
POST_MUTATION_JOB_MAP = {
    "Build exact signed Linux repository trees": LINUX_SIGNING_JOB,
    "Publish exact Homebrew tap formula": "Publish exact Homebrew tap formula / Publish owned channel homebrew_tap",
    "Publish exact Scoop manifest": "Publish exact Scoop manifest / Publish owned channel scoop",
    "Publish exact Web Share WASM bytes": "Publish exact Web Share WASM bytes / Publish owned channel web_share",
}
POST_MUTATION_RESULT_CHANNELS = ("homebrew_tap", "scoop", "web_share")


def result_envelope_names(source_sha: str, release_id: int) -> set[str]:
    return {
        f"rmux-downstream-{channel}-result-envelope-{source_sha}-{release_id}"
        for channel in POST_MUTATION_RESULT_CHANNELS
    }
