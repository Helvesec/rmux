#!/usr/bin/env python3
"""Reference contracts, with explicit opt-in native qualification execution."""
from __future__ import annotations

import argparse
import ast
from collections import Counter
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import toml_reader
import provenance_archive
import poison
import reference_applicability
import web_profile

ROOT = Path(__file__).resolve().parents[2]
DOCS = Path("docs/validation")
POPULATION = {"path": "docs/validation/reference-population.json",
              "sha256": "def43660977612915fa373815110ac50b3e4bc35eaa7367ffc57f7b8abd7544f"}
IDENTITY_FIELDS = ("id", "source", "reader", "legacy_ids_sha256", "cardinality")


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":"),
                                     allow_nan=False).encode()).hexdigest()


def strings(values, label):
    require(isinstance(values, list) and values and
            all(isinstance(v, str) and v.strip() for v in values), label + ": empty/string list")
    require(len(values) == len(set(values)), label + ": duplicate")


def integer(value):
    return type(value) is int and value > 0


def sha(value, length=64):
    return isinstance(value, str) and re.fullmatch("[0-9a-f]{%d}" % length, value) is not None


def read_reference(root, reference):
    relative = Path(reference["path"])
    require(not relative.is_absolute() and ".." not in relative.parts, "reference escapes root")
    path = root / relative
    require(path.resolve().is_relative_to(root.resolve()), "reference symlink escapes root")
    raw = provenance_archive.decode(path.read_bytes(), ROOT)
    require(hashlib.sha256(raw).hexdigest() == reference["sha256"],
            "stale reference: " + str(relative))
    return raw.decode()


def frozen_source_bytes(root, reference):
    """Read cited bytes from the checker-owned source base in root's own Git history.

    The caller's SHA-256 stays the only pin: just the same path at the frozen
    base is consulted, and its bytes must match exactly. A tree that is not
    its own Git top level, or a history without that object, fails.
    """
    relative = Path(reference["path"])
    require(not relative.is_absolute() and ".." not in relative.parts, "reference escapes root")
    base = reference_population()["historical_source"]["commit"]
    require(sha(base, 40), "frozen source base identity")

    def git(*args):
        try:
            return subprocess.run(["git", "-C", str(root), *args], capture_output=True,
                                  timeout=10, check=False)
        except (OSError, subprocess.TimeoutExpired) as error:
            raise ValueError("frozen source object unavailable: " + str(relative)) from error

    top = git("rev-parse", "--show-toplevel")
    require(top.returncode == 0 and Path(top.stdout.decode().strip()).resolve() == root.resolve(),
            "frozen source needs the tree's own Git history: " + str(relative))
    blob = git("cat-file", "blob", base + ":" + relative.as_posix())
    require(blob.returncode == 0, "frozen source object unavailable: " + str(relative))
    require(hashlib.sha256(blob.stdout).hexdigest() == reference["sha256"],
            "frozen source bytes differ: " + str(relative))
    return blob.stdout


def read_cited(root, reference):
    """Exact cited bytes: the tree's copy when unchanged, else the frozen base object.

    An evolved tree no longer hides the historical reference. It gains no
    credit either: identities that must survive are checked by inventory.
    It is only ever the strict reader beneath a reviewed-delta binding, never
    a wrapper around one, so a bound file's unreviewed edit stays stale.
    """
    try:
        return read_reference(root, reference)
    except (ValueError, OSError) as error:
        try:
            return frozen_source_bytes(root, reference).decode()
        except ValueError:
            raise error


def source_ids(kind, text):
    """Read case identities only; expectations stay in the original source."""
    if kind == "jsonl":
        return [json.loads(line)["id"] for line in text.splitlines() if line.strip()]
    if kind == "benchmark":
        return [row["id"] for row in json.loads(text)["operations"]]
    if kind == "divergences":
        return [row["id"] for row in toml_reader.loads(text)["entry"]]
    if kind == "error-exit":
        return re.findall(r'^  - id: "([^"]+)"$', text, re.M)
    if kind == "rust-tests":
        # Historical source declarations, not Cargo's cfg-expanded runtime set.
        declarations = re.findall(
            r'#\[(?:test|(?:tokio|async_std)::test)(?:\([^\]]*\))?\]'
            r'\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?'
            r'(?:async\s+)?fn\s+([A-Za-z_][A-Za-z_0-9]*)\s*\(', text)
        seen = Counter()
        result = []
        for name in declarations:
            seen[name] += 1
            result.append(name + "#" + str(seen[name]))
        return result
    if kind == "python-tests":
        result = []

        def visit(nodes, parents=()):
            for node in nodes:
                if isinstance(node, (ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)):
                    if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name.startswith("test_"):
                        result.append(".".join((*parents, node.name)))
                    visit(node.body, (*parents, node.name))
        visit(ast.parse(text).body)
        return result
    if kind == "program":
        return ["whole-program"]
    raise ValueError("unknown legacy reader: " + kind)


def reference_population(root=ROOT):
    """Checker-owned reference, independent of the submitted --root tree.

    Its pin changes only with a reviewed source/mapping delta. Never derive the
    expected population from the matrix or manifest being validated.
    """
    return json.loads(read_reference(root, POPULATION))


def family_roster(families, population):
    ids = [family["id"] for family in families]
    strings(ids, "conformance families")
    require(set(ids) == set(population["family_ids"]), "frozen family roster differs")


# Checker-owned identity transitions (v1.1); a submitted manifest cannot add one.
IDENTITY_TRANSITIONS = {
    "issue8_interactive_exit.rs": {
        "path": "tests/issue8_interactive_exit.rs",
        "renamed": {"fresh_pane_title_starts_as_host_short_before_shell_updates_it#1":
                    "fresh_pane_title_starts_as_host_before_shell_updates_it#1"}},
    "window_pane_flag_semantics.rs": {
        "path": "tests/window_pane_flag_semantics.rs",
        "renamed": {"last_pane_input_flags_apply_to_the_selected_last_pane#1":
                    "last_pane_input_flags_apply_to_the_last_pane#1"}},
    "bench_screen_adapter_spec.rs": {
        "path": "tests/bench_screen_adapter_spec.rs",
        "may_be_absent": "not shipped in the published 0.11.0 source tree"},
}


def surviving_identities(root, row, legacy):
    """In an evolved tree, every legacy identity survives in order; additions gain no credit."""
    reference = row["source"]
    transition = IDENTITY_TRANSITIONS.get(row["id"], {})
    require(transition.get("path", reference["path"]) == reference["path"], "identity transition path")
    path = root / reference["path"]
    require(path.resolve().is_relative_to(root.resolve()), "reference symlink escapes root")
    if not path.exists():
        require("may_be_absent" in transition, "legacy collection absent from the tree: " + row["id"])
        return
    raw = path.read_bytes()
    if hashlib.sha256(raw).hexdigest() == reference["sha256"]:
        return
    current = iter(source_ids(row["reader"], raw.decode()))
    renamed = transition.get("renamed", {})
    require(all(renamed.get(old, old) in current for old in legacy),
            "legacy identity lost or reordered in the tree: " + row["id"])


def inventory(root, manifest, expected=None, *, contract=None):
    # An explicit independent reference permits small unit fixtures. The CLI
    # always uses the checker-pinned full population, with no override option.
    if expected is None:
        expected = reference_population()["collections"]
    require(manifest["schema"] == 1, "inventory schema")
    require(manifest["evidence_status"] == "SOURCE_IDENTITIES_NOT_EXECUTION_PROOF", "legacy status")
    collections = manifest["collections"]
    strings([row["id"] for row in collections], "collections")
    require(len({row["source"]["path"] for row in collections}) == len(collections),
            "source mapped more than once")
    mapped, observed = {}, []
    for row in collections:
        reference = row["source"]
        # read_cited stays the binding's inner strict reader: an unreviewed edit
        # of a bound file cannot fall back to its frozen historical bytes.
        text = (read_cited(root, reference) if contract is None else
                reference_applicability.read_divergence_reference(
                    root, reference, contract, read_cited, historical=True))
        ids = source_ids(row["reader"], text)
        strings(ids, row["id"])
        require(ids == row["legacy_ids"], "legacy omission/reorder/rename: " + row["id"])
        surviving_identities(root, row, ids)
        require(row["mapping"] == "collection-id/legacy-id", "mapping rule changed")
        require(set(row["contract_fields"]) == {"target", "features", "prerequisites", "observable",
                                               "risk", "neighbors", "platform", "oracle"} and
                all(isinstance(x, str) and x for x in row["contract_fields"].values()),
                "scenario contract field absent")
        observed.append({"id": row["id"], "source": row["source"], "reader": row["reader"],
                         "legacy_ids_sha256": digest(ids), "cardinality": len(ids)})
        for old in ids:
            key = row["id"] + "/" + old
            require(key not in mapped, "legacy alias: " + key)
            mapped[key] = {"collection": row["id"], "legacy_id": old,
                           "source": row["source"], "family": row["family"],
                           "status": "NOT_REQUALIFIED"}
    require(len(mapped) == manifest["cardinality"], "inventory cardinality")
    frozen = [{key: row[key] for key in IDENTITY_FIELDS} for row in expected]
    require(sorted(observed, key=lambda row: row["id"]) == sorted(frozen, key=lambda row: row["id"]),
            "historical collection/identity mapping differs")
    return mapped


def yaml_identity(text, section, field, length):
    """Read one quoted scalar in the hash-pinned YAML's exact top-level section.

    This deliberately supports that frozen format only, not arbitrary YAML.
    Missing, duplicate, nested or differently formatted identity fields fail.
    """
    blocks = re.findall(r"^" + re.escape(section) + r":\n((?:[ \t]+[^\n]*\n|\n)*)", text, re.M)
    require(len(blocks) == 1, "oracle YAML section missing/ambiguous")
    values = re.findall(r'^  ' + re.escape(field) + r': "([0-9a-f]{%d})"$' % length, blocks[0], re.M)
    require(len(values) == 1, "oracle YAML identity missing/ambiguous")
    return values[0]


def oracle_attribution(root, declared, population):
    authorities = population["oracle_authorities"]
    historical = read_reference(root, authorities["historical"])
    current = read_reference(root, authorities["current"])
    expected = {
        "path": authorities["historical"]["path"],
        "historical_source_sha": yaml_identity(historical, "authority", "tmux_source_sha", 40),
        "historical_binary_sha256": yaml_identity(historical, "authority", "frozen_tmux_binary_sha256", 64),
        "current_reference_source_sha": yaml_identity(current, "tmux_reference", "source_sha", 40),
        "current_reference_binary_sha256": yaml_identity(current, "frozen_tmux_binary_acquisition", "binary_sha256", 64),
        "disposition": "PRESERVED_STALE_ATTRIBUTION_NO_CURRENT_PARITY_CREDIT",
    }
    require(yaml_identity(current, "frozen_tmux_binary_acquisition", "source_sha", 40) ==
            expected["current_reference_source_sha"], "oracle acquisition/source mismatch")
    require(all(declared.get(key) == value for key, value in expected.items()),
            "oracle attribution differs from pinned YAML authorities")
    return expected


def classification(record):
    kind = record["classification"]
    require(kind in {"parity", "extension", "corrective-divergence", "indeterminate"},
            "unknown conformance classification")
    for field in ("owner", "invariant", "observable"):
        require(isinstance(record.get(field), str) and record[field].strip(), "missing " + field)
    strings(record["routes"], "classification routes")
    if kind in {"parity", "corrective-divergence"}:
        oracle = record["oracle"]
        require(oracle["version"] == "tmux 3.7b" and sha(oracle["source_sha"], 40)
                and sha(oracle["binary_sha256"]) and sha(oracle["capture_sha256"]), "unbound oracle")
        require(bool(oracle["case_id"]) and bool(oracle["symbol"]), "oracle case/symbol")
    if kind == "corrective-divergence":
        second = record["independent_counterproof"]
        require(second["kind"] in {"documented-contract", "safety", "data-conservation",
                                   "progress", "equivalent-route-coherence"}, "preference is not a bug")
        require(sha(second["sha256"]) and second["sha256"] != record["oracle"]["capture_sha256"],
                "second independent proof missing")
        require(second["independent_of_oracle_case"] is True and bool(second["violation"]),
                "counterproof independence")
        require(record["test"].endswith("_product_divergence") and bool(record["differential_canary"]),
                "corrective case must not claim parity")
    if kind == "indeterminate":
        require(record.get("product_pass", False) is False, "indeterminate is not product PASS")


ROW_FIELDS = ("qualification_id", "cell_id", "scenario_id", "risk", "source_sha",
              "artifact_sha256", "command_sha256", "inputs_sha256", "observable_id",
              "target", "native_os", "features_sha256", "prerequisites_sha256", "collector_sha256")
TRACE_FIELDS = (*ROW_FIELDS, "challenge_id", "run_id", "job_id", "attempt", "execution_id",
                "started_ns", "ended_ns", "stdout_sha256", "stderr_sha256")


def row_key(row):
    return tuple(row[field] for field in ("qualification_id", "cell_id", "scenario_id", "risk"))


def execution_contract(known_ids, plan, challenges, receipts, previous=(), *, synthetic=False):
    """Validate against a separately frozen plan, never a selection inferred from receipts.

    Challenges and raw envelopes must be supplied by the trusted native collector.
    This checks binding/accounting, not the authenticity of invented observations.
    """
    require(plan["schema"] == 1 and sha(plan["source_sha"], 40), "execution plan identity")
    expected = plan["expected"]
    require(isinstance(expected, list) and expected, "empty expected population")
    expected_keys = [row_key(r) for r in expected]
    require(len(set(expected_keys)) == len(expected_keys), "duplicate expected cell")
    strings(plan["qualification_ids"], "qualifications")
    require(set(plan["qualification_ids"]) <= {"Q1", "Q2"}, "unknown qualification")
    require(set(r["qualification_id"] for r in expected) == set(plan["qualification_ids"]),
            "qualification missing from plan")
    content = {}
    claims = set()
    for row in expected:
        require(all(isinstance(row.get(f), str) and row[f] for f in ROW_FIELDS), "empty plan field")
        require(row["scenario_id"] in known_ids, "unmapped scenario")
        require(row["source_sha"] == plan["source_sha"], "mixed candidate source")
        require(all(sha(row[f]) for f in ("artifact_sha256", "command_sha256", "inputs_sha256",
                                         "features_sha256", "prerequisites_sha256", "collector_sha256")),
                "unbound execution inputs")
        require(row["native_os"] in {"linux", "macos", "windows10", "windows11"}, "unqualified native OS")
        shared = row.get("shared_scenario_ids", [])
        require(isinstance(shared, list) and len(shared) == len(set(shared)), "duplicate shared scenario")
        for scenario in [row["scenario_id"], *shared]:
            require(scenario in known_ids, "unmapped shared scenario")
            claim = (row["qualification_id"], row["cell_id"], scenario, row["risk"])
            require(claim not in claims, "shared execution counted twice")
            claims.add(claim)
        content_key = (row["cell_id"], row["scenario_id"], row["risk"])
        value = tuple(row[f] for f in ("source_sha", "artifact_sha256", "command_sha256", "inputs_sha256",
                                      "target", "native_os", "features_sha256", "prerequisites_sha256",
                                      "collector_sha256", "observable_id")) + (tuple(shared),)
        require(content_key not in content or content[content_key] == value, "Q1/Q2 content differs")
        content[content_key] = value
    if set(plan["qualification_ids"]) == {"Q1", "Q2"}:
        populations = [{(r["cell_id"], r["scenario_id"], r["risk"]) for r in expected
                        if r["qualification_id"] == q} for q in ("Q1", "Q2")]
        require(populations[0] == populations[1], "Q2 population differs")
    require(len(challenges) == len(expected), "challenge cardinality")
    tickets = {}
    for ticket in challenges:
        require(ticket["plan_sha256"] == digest(plan), "challenge issued for another plan")
        require(sha(ticket["challenge_id"]) and integer(ticket["issued_ns"]), "challenge identity/time")
        require(ticket["challenge_id"] not in tickets, "challenge reused")
        require(tuple(ticket["row_key"]) in expected_keys, "challenge cell absent")
        tickets[ticket["challenge_id"]] = ticket
    require(len({tuple(t["row_key"]) for t in challenges}) == len(expected), "challenge cell duplicated")
    require(len(receipts) == len(expected), "missing/extra/nonattempted execution")
    seen_keys, seen_challenges = set(), {r["challenge_id"] for r in previous}
    seen_traces, seen_executions = {r["trace_sha256"] for r in previous}, {r["execution_id"] for r in previous}
    seen_commands = set()
    run_qualifications = {}
    for result in receipts:
        key = row_key(result)
        require(key in expected_keys and key not in seen_keys, "wrong cell or repeated scenario")
        seen_keys.add(key)
        row = expected[expected_keys.index(key)]
        require(all(result[f] == row[f] for f in ROW_FIELDS), "receipt differs from frozen row")
        require(result.get("shared_scenario_ids", []) == row.get("shared_scenario_ids", []), "undeclared sharing")
        require(result["status"] == "PASS" and result["executed"] is True and result["skip"] is False,
                "skip/fail/nonexecution is not PASS")
        require(result["synthetic"] is synthetic, "synthetic/native evidence kind mismatch")
        require(integer(result["attempt"]) and result["attempt"] == 1, "retry cannot replace qualification")
        require(all(isinstance(result[f], str) and result[f] for f in ("run_id", "job_id", "execution_id")),
                "missing run/job/execution")
        run_key = (result["job_id"], result["run_id"])
        require(run_key not in run_qualifications or
                run_qualifications[run_key] == result["qualification_id"], "Q1/Q2 run reused")
        run_qualifications[run_key] = result["qualification_id"]
        ticket = tickets.get(result["challenge_id"])
        require(ticket is not None and tuple(ticket["row_key"]) == key, "challenge belongs to another cell")
        require(result["challenge_id"] not in seen_challenges, "challenge replay")
        require(integer(result["started_ns"]) and integer(result["ended_ns"]) and
                ticket["issued_ns"] < result["started_ns"] <= result["ended_ns"], "challenge not prior")
        trace = result["trace"]
        require(set(trace) == set(TRACE_FIELDS), "trace schema")
        require(all(trace[f] == result[f] for f in TRACE_FIELDS), "trace relabelled")
        require(sha(result["stdout_sha256"]) and sha(result["stderr_sha256"]), "unbound output")
        require(digest(trace) == result["trace_sha256"], "trace digest mismatch")
        require(result["trace_sha256"] not in seen_traces and result["execution_id"] not in seen_executions,
                "copied execution proof")
        # A different case label is not a second observation of the same operation.
        fingerprint = tuple(result[f] for f in ("qualification_id", "cell_id", "artifact_sha256",
                                                "command_sha256", "inputs_sha256", "observable_id"))
        require(fingerprint not in seen_commands, "proof alias under another scenario ID")
        seen_commands.add(fingerprint)
        seen_challenges.add(result["challenge_id"])
        seen_traces.add(result["trace_sha256"])
        seen_executions.add(result["execution_id"])
    require(seen_keys == set(expected_keys), "supported cell unattempted")
    return {"accounting": "PASS", "executions": len(receipts), "synthetic": synthetic,
            "native_authenticity_or_product_verdict": False}


def validate(root=ROOT):
    contract = json.loads((root / DOCS / "reference-contracts.json").read_text())
    manifest = json.loads((root / DOCS / "legacy-scenarios.json").read_text())
    require(contract["schema"] == 1 and contract["scope"] == "PHASE0_REFERENCE_CONTRACT", "contract identity")
    population = reference_population()
    require(contract["source_base"] == population["historical_source"]["commit"] and
            contract["plan_sha256"] == population["plan_sha256"], "frozen reference provenance differs")
    families = contract["families"]
    family_roster(families, population)
    applicable, applicability = reference_applicability.resolve(root, contract, read_reference,
                                                                read_frozen=read_cited)

    def read_contract_reference(reference):
        if reference["path"] == applicable["path"]:
            require(reference["sha256"] == applicability["historical_sha256"],
                    "historical harness reference differs")
            return read_cited(root, applicable)
        # Bound divergence and packaging bytes equal history or their reviewed
        # delta; only the strict reader beneath the bindings consults the base.
        return reference_applicability.read_divergence_reference(
            root, reference, contract,
            lambda tree, ref: reference_applicability.read_packaging_reference(
                tree, ref, contract, read_cited))

    for family in families:
        for field in ("routes", "platforms", "feature_profiles", "invariants", "observables", "neighbors"):
            strings(family[field], family["id"] + ":" + field)
        require(bool(family["owner"]) and family["product_status"] == "NOT_QUALIFIED_HERE", "family owner/status")
        for reference in family["references"]:
            read_contract_reference(reference)
    for reference in contract["authorities"]:
        read_contract_reference(reference)
    oracle_attribution(root, contract["known_oracle_mismatch"], population)
    # Historical bytes preserve all 59 IDs; current extensions gain no old credit.
    mapped = inventory(root, manifest, population["collections"], contract=contract)
    require(contract["source_base"] == manifest["baseline_source"] and
            sha(contract["source_base"], 40), "mixed reference baseline")
    require(set(row["family"] for row in manifest["collections"]) <= {f["id"] for f in families},
            "orphan scenario family")
    require(contract["execution_receipt"]["row_fields"] == list(ROW_FIELDS) and
            contract["execution_receipt"]["trace_fields"] == list(TRACE_FIELDS), "receipt schema drift")
    profile = json.loads((root / DOCS / "web-reference-profile.json").read_text())
    read_reference(root, profile["protocol_authority"])
    web_profile.validate_profile(profile)
    policy = json.loads((root / DOCS / "poison-policy.json").read_text())
    require(policy["source_base"] == contract["source_base"], "mixed poison baseline")
    poison.validate_policy(root, policy)
    return {"status": "PASS_REFERENCE_CONTRACT_ONLY", "families": len(families),
            "reference_applicability": applicability,
            "legacy_scenarios": len(mapped), "poison_observations": len(policy["observed_sites"]),
            "product_conformance": "NOT_EVALUATED", "web_indistinguishability": "NOT_EVALUATED"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--campaign", type=Path, help="read frozen plan, challenges, receipts and previous JSON; no execution")
    parser.add_argument("--synthetic-fixture", action="store_true", help="accept synthetic accounting fixtures, never native credit")
    parser.add_argument("--run-qualification", type=Path, metavar="NEW_DIR",
                        help="execute all native qualification steps once; retain failures and pending OS cells")
    parser.add_argument("--check-qualification", type=Path, nargs="+", metavar="DIR",
                        help="check the complete scenario x native OS matrix from retained runner directories")
    parser.add_argument("--qualification-id", choices=("Q0", "Q1", "Q2"), default="Q0")
    parser.add_argument("--final-artifact-manifest", type=Path,
                        help="signed final smoke manifest and complete input bundle; required for Q1/Q2 run/check")
    parser.add_argument("--tmux", type=Path, help="absolute measured tmux 3.7b binary for native qualification")
    args = parser.parse_args()
    try:
        if args.run_qualification or args.check_qualification:
            require(sum(bool(x) for x in (args.campaign, args.run_qualification, args.check_qualification)) == 1,
                    "select exactly one campaign action")
            if args.run_qualification:
                require(not args.synthetic_fixture, "native execution cannot accept synthetic fixtures")
                import os
                import candidate_identity
                from qualification_campaign import run_campaign
                from qualification_recipe import default_manifest
                root = args.root.resolve()
                os.chdir(root)
                head = candidate_identity.git(root, "rev-parse", "HEAD")
                candidate_identity.source_identity(root, head)
                if args.tmux:
                    os.environ["RMUX_FROZEN_TMUX"] = str(args.tmux)
                manifest = default_manifest(root, head, args.qualification_id,
                                            final_artifact_manifest=args.final_artifact_manifest)
                result = run_campaign(root, manifest, args.run_qualification,
                                      final_artifact_manifest=args.final_artifact_manifest)
            else:
                from qualification_evidence import check_campaign
                result = check_campaign(args.root, args.check_qualification, synthetic=args.synthetic_fixture,
                                        final_artifact_manifest=args.final_artifact_manifest)
            print(json.dumps(result, sort_keys=True))
            return int(result["status"] == "FAIL")
        result = validate(args.root)
        if args.campaign:
            def read(name):
                return json.loads((args.campaign / (name + ".json")).read_text())
            manifest = json.loads((args.root / DOCS / "legacy-scenarios.json").read_text())
            result["campaign"] = execution_contract(inventory(args.root, manifest, contract=json.loads(
                (args.root / DOCS / "reference-contracts.json").read_text())), read("plan"),
                                                   read("challenges"), read("receipts"), read("previous"),
                                                   synthetic=args.synthetic_fixture)
    except (ValueError, KeyError, TypeError, OSError, RuntimeError, subprocess.SubprocessError) as error:
        print(json.dumps({"status": "FAIL", "error": str(error)}))
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
