#!/usr/bin/env python3
"""Typed policy and canonical-message helpers for signed RMUX release tags."""

from __future__ import annotations

import json
import re
import subprocess
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any

REPOSITORY_ID = 1239918790
REPOSITORY = "Helvesec/rmux"
RELEASE_APP_ID = 4339867
SIGNATURE_NAMESPACE = "git"
TAG_PATTERN = re.compile(r"^v(?P<version>[0-9]+\.[0-9]+\.[0-9]+(?P<rc>-rc\.[0-9]+)?)$")
SHA_PATTERN = re.compile(r"^[0-9a-f]{40}$")
HASH_PATTERN = re.compile(r"^[0-9a-f]{64}$")
DIGEST_PATTERN = re.compile(r"^sha256:[0-9a-f]{64}$")
INTENT_PATTERN = re.compile(r"^[A-Za-z0-9._:-]{8,128}$")
PRINCIPAL_PATTERN = re.compile(r"^[A-Za-z0-9._@+-]{3,128}$")
TRAILERS = (
    ("RMUX-Release-Intent-ID", "release_intent_id"),
    ("RMUX-Release-Kind", "release_kind"),
    ("RMUX-Source-SHA", "source_git_sha"),
    ("RMUX-Candidate-Run-ID", "candidate_run_id"),
    ("RMUX-Candidate-Manifest-Artifact-ID", "candidate_manifest_artifact_id"),
    ("RMUX-Candidate-Manifest-Artifact-Digest", "candidate_manifest_artifact_digest"),
    ("RMUX-Candidate-Manifest-SHA256", "candidate_manifest_sha256"),
    ("RMUX-Release-Policy-Root-SHA256", "release_policy_root_sha256"),
)


class PolicyError(ValueError):
    """The versioned release-tag policy or requested identity is invalid."""


@dataclass(frozen=True)
class ReleaseTagIdentity:
    release_ref: str
    release_intent_id: str
    release_kind: str
    source_git_sha: str
    candidate_run_id: int
    candidate_manifest_artifact_id: int
    candidate_manifest_artifact_digest: str
    candidate_manifest_sha256: str
    release_policy_root_sha256: str

    def validate(self) -> None:
        match = TAG_PATTERN.fullmatch(self.release_ref)
        if match is None:
            raise PolicyError("release ref must be vX.Y.Z or vX.Y.Z-rc.N")
        expected_kind = "rc" if match.group("rc") else "stable"
        if self.release_kind != expected_kind:
            raise PolicyError(
                f"release kind {self.release_kind!r} does not match {self.release_ref}"
            )
        if INTENT_PATTERN.fullmatch(self.release_intent_id) is None:
            raise PolicyError("release intent ID has an invalid format")
        if SHA_PATTERN.fullmatch(self.source_git_sha) is None:
            raise PolicyError("source SHA must be a lowercase full SHA-1")
        if self.candidate_run_id <= 0:
            raise PolicyError("candidate run ID must be positive")
        if self.candidate_manifest_artifact_id <= 0:
            raise PolicyError("candidate manifest artifact ID must be positive")
        if DIGEST_PATTERN.fullmatch(self.candidate_manifest_artifact_digest) is None:
            raise PolicyError("candidate manifest artifact digest must be sha256:<hex>")
        if HASH_PATTERN.fullmatch(self.candidate_manifest_sha256) is None:
            raise PolicyError("candidate manifest SHA-256 must be lowercase hex")
        if HASH_PATTERN.fullmatch(self.release_policy_root_sha256) is None:
            raise PolicyError("release policy root SHA-256 must be lowercase hex")

    @property
    def full_ref(self) -> str:
        return f"refs/tags/{self.release_ref}"

    @property
    def version(self) -> str:
        match = TAG_PATTERN.fullmatch(self.release_ref)
        if match is None:  # validate() reports the useful error to callers.
            raise PolicyError("invalid release ref")
        return match.group("version")

    def message(self) -> str:
        self.validate()
        values: dict[str, str | int] = {
            field: getattr(self, field) for _, field in TRAILERS
        }
        lines = [f"RMUX release {self.release_ref}", ""]
        lines.extend(f"{label}: {values[field]}" for label, field in TRAILERS)
        return "\n".join(lines) + "\n"


@dataclass(frozen=True)
class AllowedSigner:
    principal: str
    public_key: str
    fingerprint: str


@dataclass(frozen=True)
class SignerPolicy:
    path: Path
    enabled: bool
    blocker: str
    allowed_signers: tuple[AllowedSigner, ...]

    def require_enabled(self) -> None:
        if not self.enabled or not self.allowed_signers:
            raise PolicyError(
                "release tag signing is disabled: "
                + (self.blocker or "no dedicated signer is allowlisted")
            )

    def write_allowed_signers(self, destination: Path) -> None:
        self.require_enabled()
        lines = [
            f'{signer.principal} namespaces="{SIGNATURE_NAMESPACE}" '
            f"{signer.public_key}\n"
            for signer in self.allowed_signers
        ]
        destination.write_text("".join(lines), encoding="utf-8")


def _object(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise PolicyError(f"{label} must be an object")
    return value


def _exact_keys(value: dict[str, Any], expected: set[str], label: str) -> None:
    if set(value) != expected:
        raise PolicyError(
            f"{label} keys drifted: expected {sorted(expected)}, got {sorted(value)}"
        )


def load_signer_policy(path: Path) -> SignerPolicy:
    try:
        root = _object(json.loads(path.read_text(encoding="utf-8")), "signer policy")
    except (OSError, json.JSONDecodeError) as error:
        raise PolicyError(f"cannot read signer policy {path}: {error}") from error
    _exact_keys(
        root,
        {"schema_version", "status", "repository", "release_app", "tag_policy"},
        "signer policy",
    )
    if root["schema_version"] != 1:
        raise PolicyError("signer policy schema_version must equal 1")
    repository = _object(root["repository"], "repository")
    if repository != {"id": REPOSITORY_ID, "full_name": REPOSITORY}:
        raise PolicyError("signer policy repository identity drifted")
    release_app = _object(root["release_app"], "release_app")
    if release_app != {
        "app_id": RELEASE_APP_ID,
        "may_create_only": "refs/tags/v*",
        "force_updates_allowed": False,
    }:
        raise PolicyError("release App policy drifted")
    tag_policy = _object(root["tag_policy"], "tag_policy")
    _exact_keys(
        tag_policy,
        {
            "signature_format",
            "signature_namespace",
            "ref_pattern",
            "required_private_key_secret",
            "enabled",
            "blocker",
            "allowed_signers",
        },
        "tag_policy",
    )
    if (
        tag_policy["signature_format"] != "ssh"
        or tag_policy["signature_namespace"] != SIGNATURE_NAMESPACE
        or tag_policy["ref_pattern"]
        != (r"^refs/tags/v[0-9]+\.[0-9]+\.[0-9]+(?:-rc\.[0-9]+)?$")
        or tag_policy["required_private_key_secret"] != "RMUX_RELEASE_SSH_SIGNING_KEY"
        or type(tag_policy["enabled"]) is not bool
        or not isinstance(tag_policy["blocker"], str)
    ):
        raise PolicyError("tag signing policy invariants drifted")
    raw_signers = tag_policy["allowed_signers"]
    if not isinstance(raw_signers, list):
        raise PolicyError("allowed_signers must be an array")
    signers: list[AllowedSigner] = []
    for index, raw in enumerate(raw_signers):
        signer = _object(raw, f"allowed_signers[{index}]")
        _exact_keys(signer, {"principal", "public_key", "fingerprint"}, "signer")
        principal = signer["principal"]
        public_key = signer["public_key"]
        fingerprint = signer["fingerprint"]
        if (
            not isinstance(principal, str)
            or PRINCIPAL_PATTERN.fullmatch(principal) is None
        ):
            raise PolicyError("signer principal has an invalid format")
        if (
            not isinstance(public_key, str)
            or not public_key.startswith(
                ("ssh-ed25519 ", "sk-ssh-ed25519@openssh.com ")
            )
            or "\n" in public_key
        ):
            raise PolicyError("only one-line Ed25519 SSH public keys are allowed")
        if not isinstance(fingerprint, str) or not fingerprint.startswith("SHA256:"):
            raise PolicyError("signer fingerprint must use SHA256")
        actual = ssh_public_key_fingerprint(public_key)
        if actual != fingerprint:
            raise PolicyError(
                f"signer fingerprint mismatch: expected {fingerprint}, got {actual}"
            )
        signers.append(AllowedSigner(principal, public_key, fingerprint))
    if len({signer.principal for signer in signers}) != len(signers):
        raise PolicyError("signer principals must be unique")
    if len({signer.fingerprint for signer in signers}) != len(signers):
        raise PolicyError("signer fingerprints must be unique")
    enabled = tag_policy["enabled"]
    status = root["status"]
    if not isinstance(status, str):
        raise PolicyError("signer policy status must be a string")
    if enabled and (status != "enabled" or not signers or tag_policy["blocker"]):
        raise PolicyError("enabled signer policy must have signers and no blocker")
    if not enabled and (status == "enabled" or signers):
        raise PolicyError("disabled signer policy cannot allow a signing key")
    return SignerPolicy(path, enabled, tag_policy["blocker"], tuple(signers))


def ssh_public_key_fingerprint(public_key: str) -> str:
    result = subprocess.run(
        ["ssh-keygen", "-l", "-f", "-", "-E", "sha256"],
        input=public_key + "\n",
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        raise PolicyError(f"invalid SSH public key: {result.stderr.strip()}")
    fields = result.stdout.split()
    if len(fields) < 2:
        raise PolicyError("ssh-keygen returned no public-key fingerprint")
    return fields[1]


def parse_message(message: str) -> ReleaseTagIdentity:
    if "\r" in message or "\0" in message or not message.endswith("\n"):
        raise PolicyError("tag message must be LF-only UTF-8 with one final newline")
    lines = message.splitlines()
    if len(lines) != len(TRAILERS) + 2 or lines[1] != "":
        raise PolicyError("tag message does not have the canonical line count")
    title = lines[0]
    if not title.startswith("RMUX release "):
        raise PolicyError("tag message has a non-canonical title")
    release_ref = title.removeprefix("RMUX release ")
    values: dict[str, str] = {}
    for line, (expected_label, field) in zip(lines[2:], TRAILERS, strict=True):
        prefix = f"{expected_label}: "
        if not line.startswith(prefix):
            raise PolicyError(f"expected unique canonical trailer {expected_label}")
        value = line.removeprefix(prefix)
        if not value or value != value.strip():
            raise PolicyError(f"trailer {expected_label} has an invalid value")
        values[field] = value
    try:
        identity = ReleaseTagIdentity(
            release_ref=release_ref,
            release_intent_id=values["release_intent_id"],
            release_kind=values["release_kind"],
            source_git_sha=values["source_git_sha"],
            candidate_run_id=int(values["candidate_run_id"]),
            candidate_manifest_artifact_id=int(
                values["candidate_manifest_artifact_id"]
            ),
            candidate_manifest_artifact_digest=values[
                "candidate_manifest_artifact_digest"
            ],
            candidate_manifest_sha256=values["candidate_manifest_sha256"],
            release_policy_root_sha256=values["release_policy_root_sha256"],
        )
    except ValueError as error:
        raise PolicyError("numeric tag trailers must be positive integers") from error
    identity.validate()
    if identity.message() != message:
        raise PolicyError("tag message is not byte-for-byte canonical")
    return identity


def verify_ssh_signature(policy: SignerPolicy, payload: bytes, signature: bytes) -> str:
    policy.require_enabled()
    if not payload or not signature.startswith(b"-----BEGIN SSH SIGNATURE-----\n"):
        raise PolicyError("tag has no canonical SSH signature")
    with tempfile.TemporaryDirectory(prefix="rmux-release-signature-") as directory:
        root = Path(directory)
        allowed = root / "allowed_signers"
        signature_path = root / "signature"
        policy.write_allowed_signers(allowed)
        signature_path.write_bytes(signature)
        verified: list[str] = []
        for signer in policy.allowed_signers:
            result = subprocess.run(
                [
                    "ssh-keygen",
                    "-Y",
                    "verify",
                    "-f",
                    str(allowed),
                    "-I",
                    signer.principal,
                    "-n",
                    SIGNATURE_NAMESPACE,
                    "-s",
                    str(signature_path),
                ],
                input=payload,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )
            if result.returncode == 0:
                verified.append(signer.principal)
    if len(verified) != 1:
        raise PolicyError(
            f"tag signature must match exactly one allowlisted signer, got {verified}"
        )
    return verified[0]
