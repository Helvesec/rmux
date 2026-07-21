"""Static workflow constraints for downstream release channels."""

from __future__ import annotations

from pathlib import Path


def _workflow_paths(root: Path) -> tuple[Path, Path, Path]:
    workflows = root / ".github/workflows"
    return (
        workflows / "release-downstream.yml",
        workflows / "release-chocolatey-retry.yml",
        workflows / "release-snap-retry.yml",
    )


def _worker_paths(root: Path) -> tuple[Path, ...]:
    workflows = root / ".github/workflows"
    names = (
        "release-channel-summary.yml",
        "release-chocolatey-channel.yml",
        "release-crates-channel.yml",
        "release-downstream-prepare.yml",
        "release-linux-repository-build.yml",
        "release-linux-repository-publish.yml",
        "release-owned-repository-channel.yml",
        "release-policy-channel.yml",
        "release-rmux-io-channel.yml",
        "release-rmux-io-payload.yml",
        "release-snap-channel.yml",
    )
    return tuple(workflows / name for name in names)


def _validate_reusable_workflow(path: Path, *, require_repository_guard: bool) -> None:
    text = path.read_text(encoding="utf-8")
    if "on:\n  workflow_call:" not in text or "permissions: {}" not in text:
        raise ValueError(f"{path.name} must remain reusable and default-deny")
    for forbidden in (
        "\n  push:",
        "\n  workflow_dispatch:",
        "larger-runner",
    ):
        if forbidden in text:
            raise ValueError(f"{path.name} contains forbidden value {forbidden}")
    if "runs-on: self-hosted" in text or "\n      - self-hosted" in text:
        raise ValueError(f"{path.name} gained a self-hosted runner label")
    if require_repository_guard and (
        'test "$GITHUB_REPOSITORY" = "Helvesec/rmux"' not in text
        or 'test "$GITHUB_REPOSITORY_ID" = "1239918790"' not in text
    ):
        raise ValueError(f"{path.name} does not reject external callers")


def _calls_workflow(line: str, workflow: str) -> bool:
    normalized = line.strip().removeprefix("- ").strip()
    if not normalized.startswith("uses:"):
        return False
    target = normalized.split(":", 1)[1].strip().split()[0].strip("'\"").lower()
    relative = f"./.github/workflows/{workflow}".lower()
    absolute = f"helvesec/rmux/.github/workflows/{workflow}@".lower()
    return target == relative or target.startswith(absolute)


def _validate_callers(root: Path, downstream_paths: tuple[Path, Path, Path]) -> None:
    workflows = root / ".github/workflows"
    for path in workflows.glob("*.y*ml"):
        text = path.read_text(encoding="utf-8")
        for downstream in downstream_paths:
            if not any(
                _calls_workflow(line, downstream.name) for line in text.splitlines()
            ):
                continue
            if (
                path.name == "release-receipt.yml"
                and downstream.name == "release-downstream.yml"
                and "\n  downstream:\n" in text
            ):
                caller = text.split("\n  downstream:\n", 1)[1]
                if (
                    "if: ${{ false }}" in caller
                    and caller.count("./.github/workflows/release-downstream.yml") == 1
                ):
                    continue
            raise ValueError(f"{path.name} calls disarmed {downstream.name}")


def _validate_receipt_origin(main: str) -> None:
    receipt_origin = "--expected-workflow-path .github/workflows/release-receipt.yml"
    promotion_origin = "--expected-workflow-path .github/workflows/release-promote.yml"
    identity = 'test "$RMUX_RECEIPT_RUN_WORKFLOW_ID" = "$RMUX_RECEIPT_WORKFLOW_ID"'
    if (
        main.count(receipt_origin) != 2
        or main.count(promotion_origin) != 0
        or main.count(identity) != 1
    ):
        raise ValueError("downstream receipt artifacts must come from release-receipt")
    if main.count("verify-receipt-attestation.py") != 1:
        raise ValueError("downstream plan must verify one exact signed receipt")


def _validate_retry(path: Path) -> None:
    retry = path.read_text(encoding="utf-8")
    required_counts = {
        "verify-receipt-attestation.py": 1,
        "--expected-workflow-path .github/workflows/release-receipt.yml": 2,
        "--expected-workflow-path .github/workflows/release-promote.yml": 2,
        'test "$RMUX_RECEIPT_RUN_WORKFLOW_ID" = "$RMUX_RECEIPT_WORKFLOW_ID"': 1,
        "--deny-self-hosted-runners": 1,
        "live payload artifact identity differs": 1,
        "--include-retention": 1,
        ".github/workflows/release-downstream.yml": 1,
        "prior result is not safe for one exact retry": 1,
        "prior result started outside the original request TTL": 1,
        'request["retry_depth"] != 0': 1,
        '"request_sha256": result["request_sha256"]': 1,
        '"mutation_started": result["mutation_started"]': 1,
        '"remote_request_id": result["remote_request_id"]': 1,
    }
    if any(retry.count(needle) != count for needle, count in required_counts.items()):
        raise ValueError(f"{path.name} lost exact retry evidence validation")
    if f".github/workflows/{path.name}" in retry:
        raise ValueError(f"{path.name} must allow at most one retry")


def _validate_helper_sizes(root: Path) -> None:
    names = (
        "build-downstream-receipt-reference.py",
        "channel-policy.py",
        "channel-request.py",
        "channel-result.py",
        "channel-result-reference.py",
        "channel-summary.py",
        "downstream_channels.py",
        "downstream_result_document.py",
        "downstream_result_reference.py",
        "downstream_summary.py",
        "verify-downstream-repository.py",
        "verify-channel-result-attestation.py",
    )
    for name in names:
        path = root / "scripts/release" / name
        if len(path.read_text(encoding="utf-8").splitlines()) >= 600:
            raise ValueError(f"{name} exceeds the release helper size budget")


def _validate_channel_orchestration(main: str) -> None:
    required_calls = {
        "release-channel-summary.yml": 2,
        "release-chocolatey-channel.yml": 1,
        "release-crates-channel.yml": 1,
        "release-linux-repository-build.yml": 1,
        "release-linux-repository-publish.yml": 1,
        "release-owned-repository-channel.yml": 3,
        "release-policy-channel.yml": 4,
        "release-rmux-io-channel.yml": 1,
        "release-rmux-io-payload.yml": 1,
        "release-snap-channel.yml": 1,
    }
    for workflow, count in required_calls.items():
        target = f"uses: ./.github/workflows/{workflow}"
        if main.count(target) != count:
            raise ValueError(f"downstream call count changed for {workflow}")
    markers = tuple(
        main.find(f"\n  {job}:\n")
        for job in (
            "pre-site-summary",
            "prepare-rmux-io-handoff",
            "record-rmux-io-handoff",
            "final-channel-summary",
        )
    )
    if any(marker <= 0 for marker in markers) or not all(
        left < right for left, right in zip(markers, markers[1:])
    ):
        raise ValueError("rmux.io must remain between the two summary phases")
    site = main[markers[1] : markers[3]]
    if (
        "needs: [prepare-plan, pre-site-summary]" not in site
        or "manual rmux.io" not in site
        or "release-rmux-io-channel.yml" not in site
    ):
        raise ValueError("rmux.io lost its exact manual pre-site handoff")
    if main.count("if: ${{ false }}") != 0:
        raise ValueError("downstream graph contains an internal hard-coded stub")
    if main.count("secrets: inherit") != 8:
        raise ValueError("downstream mutation workflows lost secret inheritance")


def validate_downstream_workflows(root: Path) -> None:
    paths = _workflow_paths(root)
    for path in paths:
        _validate_reusable_workflow(path, require_repository_guard=True)
    for path in _worker_paths(root):
        _validate_reusable_workflow(path, require_repository_guard=False)
    _validate_callers(root, paths)
    main = paths[0].read_text(encoding="utf-8")
    _validate_receipt_origin(main)
    _validate_channel_orchestration(main)
    for path in paths[1:]:
        _validate_retry(path)
    verifier = root / "scripts/release/verify-receipt-attestation.py"
    if "--deny-self-hosted-runners" not in verifier.read_text(encoding="utf-8"):
        raise ValueError("receipt verifier lost its GitHub-hosted runner gate")
    _validate_helper_sizes(root)
