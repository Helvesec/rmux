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


def _retry_dispatch_path(root: Path) -> Path:
    return root / ".github/workflows/release-channel-retry.yml"


def _retry_prepare_path(root: Path) -> Path:
    return root / ".github/actions/release-channel-retry-prepare/action.yml"


def _audit_path(root: Path) -> Path:
    return root / ".github/workflows/release-downstream-audit.yml"


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
                    "if: ${{ false }}" not in caller
                    and caller.count("./.github/workflows/release-downstream.yml") == 1
                ):
                    continue
            if (
                path.name == "release-channel-retry.yml"
                and downstream.name
                in {"release-chocolatey-retry.yml", "release-snap-retry.yml"}
                and text.count(f"./.github/workflows/{downstream.name}") == 1
            ):
                continue
            raise ValueError(
                f"{path.name} has an unauthorized call to {downstream.name}"
            )


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
    channel = "chocolatey" if "chocolatey" in path.name else "snap_candidate"
    workflow_id = "316439352" if channel == "chocolatey" else "316439354"
    required = (
        "uses: ./.github/actions/release-channel-retry-prepare",
        "scripts/release/prepare-channel-retry.py verify-prepared",
        "scripts/release/assert-release-capability.py downstream_channels",
        "uses: ./.github/actions/release-channel-result",
        f'producer-workflow-id: "{workflow_id}"',
        f"producer-workflow-path: .github/workflows/{path.name}",
    )
    if any(retry.count(needle) != 1 for needle in required):
        raise ValueError(f"{path.name} lost its exact single-depth retry path")
    if "if: ${{ false }}" in retry or "rebuild" in retry.lower():
        raise ValueError(f"{path.name} regained a stub or rebuild path")


def _validate_retry_dispatch(path: Path) -> None:
    text = path.read_text(encoding="utf-8")
    if (
        "on:\n  workflow_dispatch:" not in text
        or "permissions: {}" not in text
        or "\n  push:" in text
        or "\n  workflow_call:" in text
    ):
        raise ValueError("channel retry entry point must remain dispatch-only")
    for channel, workflow in (
        ("chocolatey", "release-chocolatey-retry.yml"),
        ("snap_candidate", "release-snap-retry.yml"),
    ):
        if (
            text.count(f"uses: ./.github/workflows/{workflow}") != 1
            or text.count(f"inputs.channel == '{channel}'") != 1
        ):
            raise ValueError(f"channel retry dispatcher lost {channel}")
    if "secrets: inherit" in text:
        raise ValueError("channel retry dispatcher exposes inherited secrets")


def _validate_retry_prepare(path: Path) -> None:
    text = path.read_text(encoding="utf-8")
    required = (
        'test "$GITHUB_REPOSITORY" = "Helvesec/rmux"',
        'test "$GITHUB_REPOSITORY_ID" = "1239918790"',
        'test "$RMUX_RECEIPT_RUN_ID" = "$RMUX_PRIOR_RESULT_RUN_ID"',
        "verify-receipt-attestation.py",
        "verify-channel-result-attestation.py",
        "prepare-channel-retry.py prepare",
        "artifact-ids: ${{ steps.payload.outputs.artifact_id }}",
    )
    if any(text.count(value) != 1 for value in required):
        raise ValueError("channel retry preparer lost exact evidence binding")
    if (
        text.count("--expected-workflow-path .github/workflows/release-receipt.yml")
        != 2
    ):
        raise ValueError("channel retry preparer lost exact receipt run origins")
    if (
        "release-promote.yml" in text
        or "cargo build" in text
        or "cargo package" in text
    ):
        raise ValueError("channel retry preparer regained a wrong origin or rebuild")


def _validate_live_audit(path: Path) -> None:
    text = path.read_text(encoding="utf-8")
    required = (
        "on:\n  workflow_call:",
        "  workflow_dispatch:",
        "permissions: {}",
        "environment: release-publication",
        "permission-administration: write",
        "permission-contents: read",
        "collect-downstream-repository.py",
        "verify-downstream-repository.py fixtures",
        'test "$GITHUB_RUN_ATTEMPT" = 1',
        'test "$GITHUB_SHA" = "$RMUX_EXPECTED_SOURCE_SHA"',
    )
    if any(text.count(value) != 1 for value in required):
        raise ValueError("downstream live audit lost an exact authority gate")
    for repository in (
        "homebrew-rmux",
        "rmux-packages",
        "rmux-web-share",
        "scoop-rmux",
    ):
        if text.count(repository) != 2:
            raise ValueError(f"downstream live audit lost repository {repository}")
    if (
        "permission-contents: write" in text
        or "secrets: inherit" in text
        or "self-hosted" in text
    ):
        raise ValueError("downstream live audit gained mutation authority")


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
        "collect-downstream-repository.py",
        "verify-downstream-repository.py",
        "verify-channel-result-attestation.py",
        "prepare-channel-retry.py",
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
        "release-downstream-audit.yml": 1,
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
    if "secrets: inherit" in main:
        raise ValueError("downstream mutation workflows expose inherited secrets")
    if (
        "needs: [prepare-plan, audit-downstream-authority]" not in main
        or "uses: ./.github/workflows/release-downstream-audit.yml" not in main
    ):
        raise ValueError(
            "downstream payloads do not depend on the live authority audit"
        )


def validate_downstream_workflows(root: Path) -> None:
    paths = _workflow_paths(root)
    for path in paths:
        _validate_reusable_workflow(
            path, require_repository_guard=path.name == "release-downstream.yml"
        )
    for path in _worker_paths(root):
        _validate_reusable_workflow(path, require_repository_guard=False)
    _validate_callers(root, paths)
    main = paths[0].read_text(encoding="utf-8")
    _validate_receipt_origin(main)
    _validate_channel_orchestration(main)
    for path in paths[1:]:
        _validate_retry(path)
    _validate_retry_dispatch(_retry_dispatch_path(root))
    _validate_retry_prepare(_retry_prepare_path(root))
    _validate_live_audit(_audit_path(root))
    verifier = root / "scripts/release/verify-receipt-attestation.py"
    if "--deny-self-hosted-runners" not in verifier.read_text(encoding="utf-8"):
        raise ValueError("receipt verifier lost its GitHub-hosted runner gate")
    _validate_helper_sizes(root)
