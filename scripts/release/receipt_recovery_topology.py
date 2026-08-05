"""Exact job and artifact topology for fail-closed receipt recovery."""

from __future__ import annotations

import argparse
import re
from typing import Any

REPOSITORY_ID = 1239918790

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
APT_PUBLISH_JOB = "Publish exact signed APT and RPM repositories"
CRATES_PUBLISH_JOB = "Publish exact crates.io package set"
CRATES_PREFLIGHT_JOB = "Verify crates.io Trusted Publishing authority"
CRATES_PUBLISH_JOB_EXPANDED = (
    "Publish exact crates.io package set / Publish exact crates.io package set"
)
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
PACKAGE_WRITER_JOB_MAP = POST_MUTATION_JOB_MAP | {
    CRATES_PUBLISH_JOB_EXPANDED: CRATES_PUBLISH_JOB,
}
POST_MUTATION_RESULT_CHANNELS = ("homebrew_tap", "scoop", "web_share")
PACKAGE_FAILURE_MODES = frozenset(
    {
        "package-writer-failure",
        "package-execution-failure",
        "package-result-seal-failure",
        "crates-writer-failure",
        "crates-execution-failure",
        "crates-result-seal-failure",
    }
)
APT_SUCCESS_FAILURE_MODES = frozenset(
    {
        "crates-writer-failure",
        "crates-execution-failure",
        "crates-result-seal-failure",
    }
)
CRATES_RESULT_FAILURE_MODES = frozenset(
    {"package-result-seal-failure", "crates-result-seal-failure"}
)


def result_envelope_names(source_sha: str, release_id: int) -> set[str]:
    return {
        f"rmux-downstream-{channel}-result-envelope-{source_sha}-{release_id}"
        for channel in POST_MUTATION_RESULT_CHANNELS
    }


def result_reference_names(source_sha: str, release_id: int) -> set[str]:
    return {
        f"rmux-downstream-{channel}-result-reference-{source_sha}-{release_id}"
        for channel in POST_MUTATION_RESULT_CHANNELS
    }


def job_steps(job: dict[str, Any], label: str) -> dict[str, str]:
    steps = job.get("steps")
    if not isinstance(steps, list):
        raise ValueError(f"{label} has no step list")
    result: dict[str, str] = {}
    for step in steps:
        if not isinstance(step, dict):
            raise ValueError(f"{label} has a malformed step")
        name = step.get("name")
        conclusion = step.get("conclusion")
        if not isinstance(name, str) or not isinstance(conclusion, str):
            raise ValueError(f"{label} step identity is malformed")
        if name in result and result[name] != conclusion:
            raise ValueError(f"{label} duplicate step conclusions differ")
        result[name] = conclusion
    return result


def exact_step(steps: dict[str, str], name: str, conclusion: str, label: str) -> None:
    if steps.get(name) != conclusion:
        raise ValueError(f"{label} step {name} did not conclude {conclusion}")


def canonical_failed_jobs(
    jobs: dict[str, dict[str, Any]],
) -> tuple[dict[str, dict[str, Any]], bool]:
    downstream_jobs = (
        PREPARATION_SUCCESS_JOBS
        | {LINUX_SIGNING_JOB}
        | OWNED_WRITER_JOBS
        | SKIPPED_AFTER_FAILURE
    )
    legacy_names = DIRECT_SUCCESS_JOBS | {
        f"{LEGACY_DOWNSTREAM_PREFIX}{name}" for name in downstream_jobs
    }
    direct_names = (
        DIRECT_SUCCESS_JOBS
        | {f"{PREPARATION_PREFIX}{name}" for name in PREPARATION_SUCCESS_JOBS}
        | {LINUX_SIGNING_JOB}
        | OWNED_WRITER_JOBS
        | SKIPPED_AFTER_FAILURE
    )
    post_mutation_names = (
        DIRECT_SUCCESS_JOBS
        | {f"{PREPARATION_PREFIX}{name}" for name in PREPARATION_SUCCESS_JOBS}
        | set(POST_MUTATION_JOB_MAP)
        | POST_MUTATION_SKIPPED_AFTER_FAILURE
    )
    package_writer_names = (
        DIRECT_SUCCESS_JOBS
        | {f"{PREPARATION_PREFIX}{name}" for name in PREPARATION_SUCCESS_JOBS}
        | set(PACKAGE_WRITER_JOB_MAP)
        | (POST_MUTATION_SKIPPED_AFTER_FAILURE - {CRATES_PUBLISH_JOB})
    )
    package_execution_names = package_writer_names | {CRATES_PREFLIGHT_JOB}
    raw_names = set(jobs)
    if raw_names == legacy_names:
        return (
            {
                name.removeprefix(LEGACY_DOWNSTREAM_PREFIX): job
                for name, job in jobs.items()
            },
            False,
        )
    if raw_names == direct_names:
        return (
            {name.removeprefix(PREPARATION_PREFIX): job for name, job in jobs.items()},
            False,
        )
    if raw_names == post_mutation_names:
        return (
            {
                POST_MUTATION_JOB_MAP.get(
                    name.removeprefix(PREPARATION_PREFIX),
                    name.removeprefix(PREPARATION_PREFIX),
                ): job
                for name, job in jobs.items()
            },
            True,
        )
    if frozenset(raw_names) in {
        frozenset(package_writer_names),
        frozenset(package_execution_names),
    }:
        return (
            {
                PACKAGE_WRITER_JOB_MAP.get(
                    name.removeprefix(PREPARATION_PREFIX),
                    name.removeprefix(PREPARATION_PREFIX),
                ): job
                for name, job in jobs.items()
            },
            True,
        )
    raise ValueError("failed downstream job topology differs")


def exact_job_shape(
    job: dict[str, Any], *, name: str, conclusion: str, steps: dict[str, str]
) -> None:
    if job.get("conclusion") != conclusion or job_steps(job, name) != steps:
        raise ValueError(f"failed downstream job {name} differs")


def verify_skipped_jobs(
    jobs: dict[str, dict[str, Any]], names: set[str] | frozenset[str]
) -> None:
    for name in names:
        if jobs[name].get("conclusion") != "skipped" or jobs[name].get("steps") != []:
            raise ValueError(f"post-failure downstream job {name} was not untouched")


def verify_failed_downstream_jobs(value: dict[str, Any]) -> str:
    jobs = value.get("jobs")
    if not isinstance(jobs, list) or value.get("total_count") != len(jobs):
        raise ValueError("failed downstream job set is malformed")
    by_name: dict[str, dict[str, Any]] = {}
    for job in jobs:
        if not isinstance(job, dict) or not isinstance(job.get("name"), str):
            raise ValueError("failed downstream job identity is malformed")
        name = job["name"]
        if name in by_name:
            raise ValueError("failed downstream job names are not unique")
        by_name[name] = job
    by_name, post_mutation = canonical_failed_jobs(by_name)
    for name in EARLY_SUCCESS_JOBS:
        if by_name[name].get("conclusion") != "success":
            raise ValueError(f"failed downstream prerequisite {name} is not successful")
    linux = by_name[LINUX_SIGNING_JOB]
    if post_mutation:
        checkout = "Run actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5"
        post_checkout = f"Post {checkout}"
        exact_job_shape(
            linux,
            name=LINUX_SIGNING_JOB,
            conclusion="success",
            steps={
                "Set up job": "success",
                checkout: "success",
                "Run ./.github/actions/release-linux-repository-build": "success",
                post_checkout: "success",
                "Complete job": "success",
            },
        )
        action = "Run ./.github/actions/release-owned-repository-publish"
        writer_prefix = {
            "Set up job": "success",
            checkout: "success",
            post_checkout: "success",
            "Complete job": "success",
        }
        owned_conclusions = {
            by_name[name].get("conclusion") for name in OWNED_WRITER_JOBS
        }
        if owned_conclusions == {"failure"}:
            verify_skipped_jobs(by_name, POST_MUTATION_SKIPPED_AFTER_FAILURE)
            for name in OWNED_WRITER_JOBS:
                exact_job_shape(
                    by_name[name],
                    name=name,
                    conclusion="failure",
                    steps=writer_prefix
                    | {action: "failure", f"Post {action}": "success"},
                )
            return "owned-writer-failure"
        if owned_conclusions != {"success"}:
            raise ValueError("owned repository writer conclusions differ")
        for name in OWNED_WRITER_JOBS:
            exact_job_shape(
                by_name[name],
                name=name,
                conclusion="success",
                steps=writer_prefix | {action: "success", f"Post {action}": "success"},
            )
        preflight = by_name.get(CRATES_PREFLIGHT_JOB)
        if preflight is not None:
            exact_job_shape(
                preflight,
                name=CRATES_PREFLIGHT_JOB,
                conclusion="success",
                steps={
                    "Set up job": "success",
                    "Exchange and revoke a preflight crates.io token": "success",
                    "Post Exchange and revoke a preflight crates.io token": "success",
                    "Complete job": "success",
                },
            )
        verify_skipped_jobs(
            by_name,
            POST_MUTATION_SKIPPED_AFTER_FAILURE - {APT_PUBLISH_JOB, CRATES_PUBLISH_JOB},
        )
        apt_action = "Run ./.github/actions/release-linux-repository-publish"
        apt = by_name[APT_PUBLISH_JOB]
        apt_conclusion = apt.get("conclusion")
        if apt_conclusion not in {"success", "failure"}:
            raise ValueError("APT and RPM repository writer conclusion differs")
        exact_job_shape(
            apt,
            name=APT_PUBLISH_JOB,
            conclusion=apt_conclusion,
            steps=writer_prefix
            | {apt_action: apt_conclusion, f"Post {apt_action}": "success"},
        )

        crates = by_name[CRATES_PUBLISH_JOB]
        if crates.get("conclusion") != "failure":
            raise ValueError("crates.io package writer did not fail closed")
        actual_crates_steps = job_steps(crates, CRATES_PUBLISH_JOB)
        release_checkout = "Check out the exact release source"
        post_release_checkout = f"Post {release_checkout}"

        def crates_steps(
            *, auth: str, writer: str, outcome: str, seal: str
        ) -> tuple[dict[str, str], dict[str, str]]:
            common = {
                "Set up job": "success",
                checkout: "success",
                "Run ./.github/actions/release-channel-prepare": "success",
                "Resolve exact crates.io execution authority": "success",
                "Exchange GitHub OIDC for a short-lived crates.io token": auth,
                "Publish and redownload every exact crate": writer,
                "Normalize executable and policy-only outcomes": outcome,
                "Seal exact crates.io result evidence": seal,
                "Post Exchange GitHub OIDC for a short-lived crates.io token": "success",
                post_checkout: "success",
                "Complete job": "success",
            }
            with_release_checkout = common | {
                release_checkout: "success",
                post_release_checkout: "success",
            }
            return common, with_release_checkout

        modes = {
            "writer-failure": crates_steps(
                auth="failure", writer="skipped", outcome="skipped", seal="skipped"
            ),
            "execution-failure": crates_steps(
                auth="success", writer="failure", outcome="skipped", seal="skipped"
            ),
            "result-seal-failure": crates_steps(
                auth="success", writer="success", outcome="success", seal="failure"
            ),
        }
        matching_modes = [
            mode
            for mode, allowed_steps in modes.items()
            if actual_crates_steps in allowed_steps
        ]
        if len(matching_modes) != 1:
            raise ValueError("failed downstream job crates.io package writer differs")
        prefix = "crates" if apt_conclusion == "success" else "package"
        return f"{prefix}-{matching_modes[0]}"
    verify_skipped_jobs(by_name, SKIPPED_AFTER_FAILURE)
    if linux.get("conclusion") != "failure":
        raise ValueError("Linux repository signing did not fail closed")
    linux_steps = job_steps(linux, LINUX_SIGNING_JOB)
    exact_step(
        linux_steps,
        "Import distinct repository signing keys",
        "failure",
        LINUX_SIGNING_JOB,
    )
    for name in (
        "Authenticate retained history and generate signed repositories",
        "Add static package host files and exact checksum inventory",
        "Upload the exact signed repository tree",
    ):
        exact_step(linux_steps, name, "skipped", LINUX_SIGNING_JOB)

    for name in OWNED_WRITER_JOBS:
        job = by_name[name]
        if job.get("conclusion") != "failure":
            raise ValueError(f"owned repository job {name} did not fail closed")
        steps = job_steps(job, name)
        exact_step(
            steps,
            "Mint a repository-scoped downstream writer token",
            "failure",
            name,
        )
        exact_step(
            steps,
            "Publish and reread exact repository bytes",
            "skipped",
            name,
        )
        exact_step(
            steps,
            "Seal exact owned repository result evidence",
            "skipped",
            name,
        )
    return "pre-mutation-failure"


def verify_failed_downstream_artifacts(
    value: dict[str, Any],
    args: argparse.Namespace,
    failed_control_sha: str,
    failure_mode: str,
) -> int:
    artifacts = value.get("artifacts")
    if not isinstance(artifacts, list) or value.get("total_count") != len(artifacts):
        raise ValueError("failed downstream artifact set is malformed")
    expected_names = {
        f"rmux-publication-receipt-{args.source_sha}-{args.release_id}",
        f"rmux-publication-receipt-envelope-{args.source_sha}-{args.release_id}",
        f"rmux-downstream-authority-{args.source_sha}-{args.failed_run_id}",
        f"rmux-downstream-plan-{args.source_sha}-{args.release_id}",
        *(
            f"rmux-downstream-{channel}-payload-{args.source_sha}-{args.release_id}"
            for channel in PAYLOAD_CHANNELS
        ),
    }
    allowed_names = (expected_names,)
    if failure_mode != "pre-mutation-failure":
        expected_names.add(
            f"rmux-downstream-apt_rpm-signed-{args.source_sha}-{args.release_id}"
        )
        expected_names.update(
            f"rmux-downstream-{channel}-result-{args.source_sha}-{args.release_id}"
            for channel in POST_MUTATION_RESULT_CHANNELS
        )
        if failure_mode == "owned-writer-failure":
            allowed_names = (
                expected_names,
                expected_names
                | result_envelope_names(args.source_sha, args.release_id),
            )
        elif failure_mode in PACKAGE_FAILURE_MODES:
            expected_names.update(
                result_envelope_names(args.source_sha, args.release_id)
            )
            expected_names.update(
                result_reference_names(args.source_sha, args.release_id)
            )
            if failure_mode in APT_SUCCESS_FAILURE_MODES:
                for suffix in ("result", "result-envelope", "result-reference"):
                    expected_names.add(
                        f"rmux-downstream-apt_rpm-{suffix}-{args.source_sha}-{args.release_id}"
                    )
            if failure_mode in CRATES_RESULT_FAILURE_MODES:
                expected_names.add(
                    f"rmux-downstream-crates_io-result-{args.source_sha}-{args.release_id}"
                )
            allowed_names = (expected_names,)
        else:
            raise ValueError("failed downstream recovery mode differs")
    by_name: dict[str, dict[str, Any]] = {}
    for artifact in artifacts:
        if not isinstance(artifact, dict) or not isinstance(artifact.get("name"), str):
            raise ValueError("failed downstream artifact identity is malformed")
        name = artifact["name"]
        if name in by_name:
            raise ValueError("failed downstream artifact names are not unique")
        by_name[name] = artifact
    if set(by_name) not in allowed_names:
        raise ValueError("failed downstream artifact topology differs")
    for name, artifact in by_name.items():
        workflow_run = artifact.get("workflow_run", {})
        if (
            type(artifact.get("id")) is not int
            or artifact["id"] <= 0
            or artifact.get("expired") is not False
            or not isinstance(artifact.get("digest"), str)
            or re.fullmatch(r"sha256:[0-9a-f]{64}", artifact["digest"]) is None
            or workflow_run.get("id") != args.failed_run_id
            or workflow_run.get("head_sha") != failed_control_sha
            or workflow_run.get("head_branch") != "main"
            or workflow_run.get("repository_id") != REPOSITORY_ID
            or workflow_run.get("head_repository_id") != REPOSITORY_ID
        ):
            raise ValueError(f"failed downstream artifact {name} identity differs")
    return by_name[f"rmux-publication-receipt-{args.source_sha}-{args.release_id}"][
        "id"
    ]
