"""Read-only applicability of an exact, independently accepted harness delta.

The checker owns the policy pin. Submitted trees cannot authorize new bytes.
Archived receipts are parsed as data; no harness or native runner is imported.
"""
from datetime import datetime
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
POLICY = {"path": "docs/validation/reference-applicability.json",
          "sha256": "66763828683fb6e0f726cc6cb25775007b8aab06cb504571d8a19f20cc76f27d"}


def require(condition, message):
    if not condition:
        raise ValueError("reference applicability: " + message)


def receipt(evidence):
    raw = evidence["text"].encode()
    require(len(raw) == evidence["bytes"] and
            hashlib.sha256(raw).hexdigest() == evidence["sha256"], "receipt bytes differ")
    return json.loads(raw)


def timestamp(value):
    parsed = datetime.fromisoformat(value)
    require(parsed.utcoffset() is not None, "undated authority")
    return parsed


def validate_policy(policy):
    """Check the joins as well as the source-owned whole-record digest."""
    require(policy["schema"] == 1 and policy["scope"] == "REFERENCE_APPLICABILITY_ONLY",
            "policy scope")
    binding, evidence, proof = (policy[k] for k in ("binding", "evidence", "source_proof"))
    author, review, consumer = (receipt(evidence[k]) for k in ("author_candidate", "review", "consumer"))
    require(author["job_id"] == "L5-024" and review["job_id"] == binding["review"]["job_id"] == "R2-057" and
            consumer["job_id"] == binding["consumer"]["job_id"] == "integrator-068" and
            consumer["author_job"] == author["job_id"], "authority identity differs")
    for owner, key, target in ((review["candidate"], "receipt", "author_candidate"),
                               (consumer, "review", "review")):
        require(owner[key] == {k: evidence[target][k] for k in ("path", "bytes", "sha256")},
                "unbound receipt authority")
    require(review["verdict"] == "ACCEPT" and review["component_decision"] == "ACCEPT_BOUNDED" and
            consumer["status"] == "INTEGRATED_ACCEPTED_BOUNDED_HARNESS_COMPONENT" and
            not review["whole_criterion_closed"] and not consumer["criterion_closed"], "acceptance scope")
    require(author["commit"] == review["candidate"]["commit"] == consumer["candidate"] and
            author["tree"] == review["candidate"]["tree"] == consumer["candidate_tree"], "unbound candidate source")
    path = binding["path"]
    pin = {"path": str(Path(author["repository"]) / path),
           "bytes": proof["integrated"]["bytes"], "sha256": binding["current"]["sha256"]}
    require(pin in author["source_pins"] and path in consumer["paths"], "unbound current source")
    require(binding["current"]["commit"] == consumer["integrated_commit"] == proof["integrated"]["commit"] and
            binding["current"]["tree"] == consumer["integrated_tree"] == proof["integrated"]["tree"] and
            binding["historical"]["commit"] == consumer["parent"] == policy["integrated_parent"] ==
            proof["historical"]["commit"], "unbound integrated source")
    require(policy["integrated_ancestor_of_qualified_base"] is True and
            all(binding["qualified_base"][k] == proof["qualified_base"][k] for k in ("commit", "tree")) and
            all(proof["integrated"][k] == proof["qualified_base"][k] for k in ("sha256", "blob", "bytes")) and
            all(p["path"] == path for p in proof.values()) and
            proof["historical"]["sha256"] == binding["historical"]["sha256"] and
            proof["integrated"]["sha256"] == binding["current"]["sha256"], "unbound source proof")
    require(binding["review"]["completed_at"] == review["completed_at"] and
            binding["consumer"]["at"] == consumer["at"] and
            all(binding[k]["receipt_sha256"] == evidence[k]["sha256"] for k in ("review", "consumer")),
            "unbound authority date or digest")
    require(timestamp(author["end"]) <= timestamp(review["completed_at"]) <=
            timestamp(proof["integrated"]["commit_date"]) <= timestamp(consumer["at"]) <=
            timestamp(proof["qualified_base"]["commit_date"]), "authority chronology")
    return binding


def validate_prospective(policy):
    """Join the reviewed source delta without inventing an integration receipt.

    The original integrated chain stays intact. This additional entry permits
    reference reading only; exact companion bytes and an explicit declaration
    are required by resolve. It grants no native or whole-harness acceptance.
    """
    entry = policy["prospective"]
    binding, evidence, proof = (entry[k] for k in ("binding", "evidence", "source_proof"))
    previous = {k: v for k, v in policy.items() if k != "prospective"}
    previous_sha = hashlib.sha256((json.dumps(previous, indent=2) + "\n").encode()).hexdigest()
    require(binding["previous_policy_sha256"] == previous_sha and
            binding["previous_current_sha256"] == policy["binding"]["current"]["sha256"],
            "prospective prior policy differs")
    values = {k: receipt(v) for k, v in evidence.items()}
    author, result, review = (values[k] for k in ("author_candidate", "author_result", "review"))
    source_review, attempts = (values[k] for k in ("source_review", "attempts"))
    require(author["job_id"] == result["job_id"] == "L5-028" and
            review["job_id"] == source_review["job_id"] == attempts["job_id"] ==
            binding["review"]["job_id"] == "R2-071", "prospective authority differs")
    for actual, key in ((result["candidate"], "author_candidate"),
                         (review["candidate"]["source_review"], "source_review"),
                         (review["attempt_accounting"]["reconciliation"], "attempts")):
        require(actual == {k: evidence[key][k] for k in ("path", "bytes", "sha256")},
                "prospective receipt authority differs")
    for key, previous_key in (("preserved_prior_review", "review"),
                              ("preserved_integration", "consumer")):
        require(review["component_acceptance"][key] ==
                {k: policy["evidence"][previous_key][k] for k in ("path", "bytes", "sha256")},
                "prospective prior acceptance differs")
    require(review["verdict"] == "ACCEPT" and review["component_decision"] == "ACCEPT_BOUNDED" and
            review["independent_review"] is True and review["whole_criterion_closed"] is False and
            review["integration"]["authorized"] is False and
            source_review["status"] == "ACCEPT_BOUNDED_COMPONENT_PENDING_CONTRACT_APPLICABILITY" and
            binding["scope"] == "REVIEWED_SOURCE_REFERENCE_ONLY" and binding["native_credit"] is False and
            binding["integration"] == "PENDING_INDEPENDENT_APPLICABILITY_REVIEW_AND_UNIQUE_INTEGRATOR" and
            entry["no_future_integration_receipt_invented"] is True, "prospective acceptance scope")
    candidate = binding["candidate"]
    for field in ("commit", "tree"):
        require(candidate[field] == author[field] == result["source"][field] ==
                review["candidate"][field] == source_review["candidate"][field],
                "prospective candidate source differs")
    require(binding["path"] == policy["binding"]["path"] and
            binding["review"]["completed_at"] == review["completed_at"] and
            binding["review"]["receipt_sha256"] == evidence["review"]["sha256"],
            "prospective review date or digest differs")
    files = binding["source_files"]
    require(len(files) == 6 and len({p["path"] for p in files}) == len(files) and
            [p["path"] for p in files] == author["changed_paths"] == review["candidate"]["changed_paths"] ==
            source_review["candidate"]["changed_paths"], "prospective source population differs")
    require(files == [{k: p[k] for k in ("path", "bytes", "sha256")} for p in proof["candidate_files"]],
            "prospective file proof differs")
    for item, source in zip(files, proof["candidate_files"]):
        rel = Path(item["path"])
        require(not rel.is_absolute() and ".." not in rel.parts and
                {"path": str(Path(author["repository"]) / rel), "bytes": item["bytes"],
                 "sha256": item["sha256"]} in author["source_pins"] and
                all(source[k] == candidate[k] for k in ("commit", "tree")),
                "prospective source file unbound")
        require(timestamp(source["commit_date"]) <= timestamp(author["at"]) <=
                timestamp(result["at"]) <= timestamp(review["completed_at"]), "prospective chronology")
    harness = next(p for p in files if p["path"] == binding["path"])
    require(all(harness[k] == candidate[k] for k in ("bytes", "sha256")) and
            proof["qualified_base"]["sha256"] == binding["previous_current_sha256"] and
            proof["qualified_base"]["path"] == binding["path"] and
            all(proof["qualified_base"][k] == binding["qualified_base"][k] for k in ("commit", "tree")),
            "prospective qualified source differs")
    delta = review["component_acceptance"]["source_ast_delta"][binding["path"]]
    require(entry["reviewed_ast_delta"] == delta == source_review["semantic_delta"][binding["path"]] and
            delta["changed"] == ["qualification_errors"] and delta["removed"] == [] and
            set(delta["added"]) == {"quiet_window_errors", "role_graph_errors"} and
            {p["symbol"] for p in entry["semantic_mapping"]} == set(delta["changed"] + delta["added"]),
            "prospective semantic mapping differs")
    limits = entry["limits"]
    failed = [r["line_id"] for r in review["line_verdicts"] if r["reviewer_status"] == "FAIL"]
    roles = review["role_and_source"]
    require(limits["remaining"] == failed == ["0.harness.thermal", "0.harness.identity",
            "0.harness.residual", "0.harness.baseline"] and
            limits["harness_PASS"] == review["line_summary"]["PASS"] == 8 and
            limits["harness_FAIL"] == review["line_summary"]["FAIL"] == 4 and
            limits["R5"] == review["R5"] == "FAIL" and limits["R6"] == review["R6"] == "FAIL" and
            limits["complete_history"] == roles["full_history"] is False and
            limits["package_source"] == roles["package_actual_source"] ==
            result["package_provenance"]["installed_package_original_source"] and
            limits["package_tree"] == roles["package_actual_tree"] and
            limits["package_git_dirty"] == roles["git_dirty"] is True and
            limits["release_artifact"] == roles["release_artifact"] is False,
            "prospective native limits differ")
    require(limits["qualification_attempts"] == attempts["qualification_units_total"] ==
            review["attempt_accounting"]["qualification_canary_version_attempts"] == 24 and
            limits["driver_calls"] == attempts["raw_command_accounting"]["all_recorded_driver_calls"] ==
            review["attempt_accounting"]["all_recorded_driver_calls"] == 162 and
            limits["qualification_cap"] == attempts["cap"] == 128 and
            limits["attempt_unit_rewritten"] == attempts["attempt_limit_rewritten"] is False,
            "prospective attempt accounting differs")
    accepted = values["accepted_contract"]
    require(accepted["job_id"] == "integrator-081" and accepted["criterion"] == "0.contracts" and
            accepted["status"] == "INTEGRATED_AND_CREDITED_REFERENCE_CONTRACTS" and
            accepted["line_PASS"] == 12 and accepted["line_FAIL"] == 0,
            "accepted contract receipt differs")
    return binding


def resolve(root, contract, read_reference, checker_root=ROOT, *, read_frozen=None):
    """Select historical/current bytes without changing either historical locator.

    With read_frozen, an unreviewed later harness is neither read nor credited:
    the exact historical bytes from the frozen source base stay the reference.
    """
    policy = json.loads(read_reference(checker_root, POLICY))
    binding = validate_policy(policy)
    prospective = validate_prospective(policy)
    path = binding["path"]
    historical = {"path": path, "sha256": binding["historical"]["sha256"]}
    require(contract["source_base"] == binding["historical"]["commit"], "historical source differs")
    # Bind both original semantic locators, independent of family row order.
    authorities = [r for r in contract["authorities"] if Path(r["path"]) == Path(path)]
    benchmark = next(f for f in contract["families"] if f["id"] == "benchmark")
    references = [r for r in benchmark["references"] if Path(r["path"]) == Path(path)]
    require(authorities == [historical] and references == [historical], "historical locator altered")
    declaration = contract.get("reference_applicability")
    require(declaration is None or declaration == binding, "unbound authority/date/source declaration")
    new_declaration = contract.get("prospective_reference_applicability")
    require(new_declaration is None or new_declaration == prospective,
            "unbound prospective authority/date/source declaration")
    source = root / path
    require(source.resolve().is_relative_to(root.resolve()), "harness symlink escapes root")
    observed = hashlib.sha256(source.read_bytes()).hexdigest()
    if observed == historical["sha256"]:
        mode, accepted = "HISTORICAL", binding["historical"]
    elif observed == prospective["candidate"]["sha256"]:
        require(declaration == binding and new_declaration == prospective,
                "reviewed source requires prospective declaration")
        for item in prospective["source_files"]:
            raw = read_reference(root, {k: item[k] for k in ("path", "sha256")})
            require(len(raw.encode()) == item["bytes"], "reviewed companion size differs")
        mode, accepted = "REVIEWED_PROSPECTIVE", prospective["candidate"]
    elif observed == binding["current"]["sha256"]:
        require(declaration == binding, "current source requires authority/date/source declaration")
        mode, accepted = "ACCEPTED_CURRENT", binding["current"]
    else:
        require(read_frozen is not None, "unauthorized current harness")
        mode, accepted = "FROZEN_HISTORICAL", binding["historical"]
    if mode == "FROZEN_HISTORICAL":
        effective = historical
        read_frozen(root, effective)
    else:
        effective = {"path": path, "sha256": observed}
        read_reference(root, effective)
    return effective, {"policy_id": (prospective if mode == "REVIEWED_PROSPECTIVE" else binding)["policy_id"], "mode": mode,
                       "historical_source": binding["historical"]["commit"],
                       "historical_sha256": historical["sha256"],
                       "observed_sha256": observed, "accepted_source": accepted["commit"],
                       "scope": policy["scope"], "native_credit": False}


# Source-owned exact applicability; the submitted contract cannot repin it.
PACKAGING_BINDING = {
    "policy_id": "packaging.sbom-tool-pins.v1",
    "scope": "REFERENCE_APPLICABILITY_ONLY",
    "path": "scripts/packaging/rmux-package-layout.json",
    "historical_sha256": "7853088e4e54068a137f64c26605f87411397985b6a15b58545765b6685ee569",
    "current_sha256": "102c8ba0867791364613b8c05a10b8937451342da83edc2e67394cdfa2c0013d",
    "delta": {
        "profiles": [
            "linux-archive-cargo-build-closure-v1",
            "linux-rpm-cargo-build-closure-v1",
            "linux-deb-cargo-build-closure-v1",
            "linux-tar-repro-cargo-build-closure-v1",
            "linux-deb-repro-cargo-build-closure-v1",
            "linux-rpm-repro-cargo-build-closure-v1",
            "linux-rpm-full-repro-cargo-build-closure-v1",
            "linux-deb-full-repro-cargo-build-closure-v1",
            "linux-tar-full-repro-cargo-build-closure-v1"
        ],
        "roles": [
            "generator",
            "validator"
        ],
        "path": "scripts/packaging/sbom.py",
        "previous_sha256": "9d065b2f82a341121461c9a918f3c9cffe5e793fcdaeac2ae44857d8250ae2ce",
        "current_sha256": "0b6decf7e4b0461ee2af0e552112333114236ead0a497c3b5713c979338d7f40",
        "pin_count": 18
    },
    "native_credit": False
}


def read_packaging_reference(root, reference, contract, read_reference):
    """Read the historical descriptor or its sole reviewed 18-tool-pin delta.

    Both complete byte sequences are pinned. Reversing exactly the listed pins
    must reproduce the historical bytes; no other field or formatting changes
    are applicable. The original locator and generic strict reader stay intact.
    """
    binding = PACKAGING_BINDING
    if reference["path"] != binding["path"]:
        return read_reference(root, reference)
    require(reference == {"path": binding["path"], "sha256": binding["historical_sha256"]},
            "historical packaging locator altered")
    source = root / binding["path"]
    require(source.resolve().is_relative_to(root.resolve()), "packaging symlink escapes root")
    raw = source.read_bytes()
    observed = hashlib.sha256(raw).hexdigest()
    if observed == binding["historical_sha256"]:
        return read_reference(root, reference)
    require(contract.get("packaging_reference_applicability") == binding,
            "unbound packaging applicability declaration")
    text = read_reference(root, {"path": binding["path"], "sha256": binding["current_sha256"]})
    delta = binding["delta"]
    previous_pin, current_pin = (delta[k].encode() for k in ("previous_sha256", "current_sha256"))
    require(raw.count(current_pin) == delta["pin_count"] and previous_pin not in raw,
            "packaging pin population differs")
    profiles = json.loads(text)["sbom"]["profiles"]
    require(list(profiles) == delta["profiles"] and
            all(profiles[profile][role] == {"path": delta["path"], "sha256": delta["current_sha256"]}
                for profile in delta["profiles"] for role in delta["roles"]),
            "packaging tool pin locations differ")
    require(hashlib.sha256(raw.replace(current_pin, previous_pin)).hexdigest() ==
            binding["historical_sha256"], "packaging delta exceeds the 18 tool pins")
    return text


# Source-owned provenance: the historical locator is never replaced by a repin.
DIVERGENCE_BINDING = {
    "policy_id": "divergences.integrated-floating-delta.v1",
    "scope": "REFERENCE_APPLICABILITY_ONLY",
    "path": "tests/reference/tmux_compat/divergences.toml",
    "historical_source": "80575f422b07e12f1399e54ebee5049dc65503e6",
    "historical_sha256": "a44f207a4ee3a06b19c8d8e92c93e1e5ee128f816b722d7ca1d2b98de133f365",
    "current_sha256": "3652c6cedbb1466dd9bcdc8ec2479a227a014afd16bb9389fa6a971f6005bf2a",
    "integrated_changes": [
        {
            "commit": "68f408611e9da15117e247a8351f64ff536c095a",
            "sha256": "0341bd13751d293ffd12b3ae45343826247bbec50d7772b00136dee52214e07a"
        },
        {
            "commit": "91bb296ea6c9ce15557f4061d4f24a02cfb43a97",
            "sha256": "3cd980389472e7e65f11e2fd69eb406f39d9e456218554181ec700a96706cadc"
        },
        {
            "commit": "f02f5d698f83a338c74c2f20c430c708ea340e36",
            "sha256": "3652c6cedbb1466dd9bcdc8ec2479a227a014afd16bb9389fa6a971f6005bf2a"
        }
    ],
    "changed_ids": [
        "C-D32"
    ],
    "added_ids": [
        "C-D93"
    ],
    "native_credit": False
}

# Later integrated ledger edits retain the original declaration and locator.
LEDGER_BINDING = {
    **DIVERGENCE_BINDING,
    "policy_id": "divergences.integrated-ledger-delta.v2",
    "current_sha256": "10e55553b2025f05f12815f1f83bf84c150537143a7126e526d82fb5baf893ec",
    "integrated_changes": DIVERGENCE_BINDING["integrated_changes"] + [
        {
            "commit": "d6e0acafd88104843c715c8efaf955a6a045d197",
            "sha256": "ae8cce32cd3b8de721ef6b7cde8c17be4b54e7e21a6e11c8c6946c4cb475d7da"
        },
        {
            "commit": "bf464d0567aac5dfc58e13d71bfb3a502a7e5cff",
            "sha256": "4686531ba1ed513cbe2be5f7e261dac38aecaf19620d22930db1ea7eb38a77b3"
        },
        {
            "commit": "77b1f7ca79be2e4759f0c2954d48bdc583c02524",
            "sha256": "52cbbab6a41adaa6025243e4e5b5dbf0fc70e511ef076470ce88a1aee79c1ffc"
        },
        {
            "commit": "a57d4cce8996b88a4dd195e5942a06d7b10aa41c",
            "sha256": "c29a87badf661c6bb61e097853a58b57423794f94a394e00e5e46cbcef31d59b"
        }
    ],
    "changed_ids": ["C-D32", "C-D36", "C-D70", "C-D85"]
}

# Evidence-only relocations reverse exactly to the v2 ledger, with no new credit.
RELOCATED_LEDGER_BINDING = {
    **LEDGER_BINDING,
    "policy_id": "divergences.evidence-relocation-delta.v3",
    "current_sha256": "ad63d9add4f18c9c3428a450cd5a713107541c1c714c852ce039c604902246eb",
    "integrated_changes": LEDGER_BINDING["integrated_changes"] + [{
        "commit": "eec9f8814358a598d5eaba30ba28912f9e02c2ad",
        "sha256": "ad63d9add4f18c9c3428a450cd5a713107541c1c714c852ce039c604902246eb",
    }],
    "changed_ids": ["C-D32", "C-D36", "C-D51", "C-D52", "C-D55", "C-D70", "C-D85", "C-D92"],
}

# One appended entry reverses exactly to the v3 ledger, with no new credit.
ADDED_ENTRY_LEDGER_BINDING = {
    **RELOCATED_LEDGER_BINDING,
    "policy_id": "divergences.added-entry-delta.v4",
    "current_sha256": "cd22dc230a3e91a452641458dd58dfe37ed10c54f77cfe7b775b63d9a10c9da0",
    "integrated_changes": RELOCATED_LEDGER_BINDING["integrated_changes"] + [{
        "commit": "4ec27dc18fe3fdf75419cdc065f806dcb946c49b",
        "sha256": "cd22dc230a3e91a452641458dd58dfe37ed10c54f77cfe7b775b63d9a10c9da0",
    }],
    "added_ids": ["C-D93", "C-D94"],
}
ADDED_C_D94 = '''
[[entry]]
id = "C-D94"
summary = "A TMUX naming another server keeps its TMUX_PANE from naming a pane here"
inventory = "bounded"
evidence = ["crates/rmux-server/src/handler/target_support.rs", "crates/rmux-server/src/handler_targets/inherited_tests.rs::requester_context_reads_tmux_pane_without_rmux_unless_tmux_names_another_server"]
tests = ["crates/rmux-server/src/handler_display_message_tests.rs::client_less_display_message_ignores_tmux_pane_of_another_server_product_divergence"]
limitations = ["tmux 3.7b reads TMUX_PANE in cmd_find_inside_pane without comparing the socket in TMUX, so a TMUX of another server still names the pane B|%1; RMUX keeps the attached client A|%0, since that pane id belongs to the other server (rmux run inside a tmux pane), also through list-panes and source-file", "Without RMUX, TMUX_PANE alone, an empty TMUX or a TMUX naming this server name the pane as in tmux 3.7b; RMUX, when present, still decides and RMUX_PANE stays bound to it"]
'''

# One edited entry reverses exactly to the v4 ledger, with no new credit.
EDITED_ENTRY_LEDGER_BINDING = {
    **ADDED_ENTRY_LEDGER_BINDING,
    "policy_id": "divergences.edited-entry-delta.v5",
    "current_sha256": "d8c7e27de1f44abe5065ea1bec7f6e9a481e616607eb82b03581d6346c136857",
    "integrated_changes": ADDED_ENTRY_LEDGER_BINDING["integrated_changes"] + [{
        "commit": "e5b9a6f90912cea605c253514d7887aa7ee05205",
        "sha256": "d8c7e27de1f44abe5065ea1bec7f6e9a481e616607eb82b03581d6346c136857",
    }],
    "changed_ids": ["C-D32", "C-D36", "C-D44", "C-D51", "C-D52", "C-D55", "C-D70", "C-D85",
                    "C-D92"],
}
EDITED_C_D44 = ('''[[entry]]
id = "C-D44"
summary = "RMUX bounds run-shell shutdown and terminates the tracked Unix process group or Windows Job Object; tmux 3.7b preserves started jobs"
inventory = "bounded"
tests = [
  "tests/unix_background_lifecycle_acceptance.rs::kill_server_terminates_direct_background_run_shell_tree_product_divergence",
  "tests/unix_background_lifecycle_acceptance.rs::kill_server_terminates_queued_background_run_shell_tree_product_divergence",
  "tests/unix_background_lifecycle_acceptance.rs::kill_server_bounds_slow_session_closed_hook_tree_product_divergence",
  "tests/unix_background_lifecycle_acceptance.rs::kill_server_joins_background_session_closed_hook_trees_product_divergence",
  "tests/control_cli_lifecycle.rs::control_client_of_a_server_stopping_two_hooked_sessions_runs_both_hooks_product_divergence",
]
limitations = [
  "Unix descendants that call setsid() and leave the tracked process group may survive shutdown",
  "RMUX drains background session-closed hooks within its bounded shutdown window; work still queued when that window expires is cancelled",
  "With two sessions and a blocking run-shell session-closed hook, tmux 3.7b races its control client exit against the first hook and usually runs only that hook (12 of 16 measured runs), so -C usually sees one %sessions-changed and -CC a varying block; on SIGTERM, SIGINT and kill-server RMUX tells the client both closes and runs both hooks",
]

''', '''[[entry]]
id = "C-D44"
summary = "RMUX bounds run-shell shutdown and terminates the tracked Unix process group or Windows Job Object; tmux 3.7b preserves started jobs"
inventory = "bounded"
tests = [
  "tests/unix_background_lifecycle_acceptance.rs::kill_server_terminates_direct_background_run_shell_tree_product_divergence",
  "tests/unix_background_lifecycle_acceptance.rs::kill_server_terminates_queued_background_run_shell_tree_product_divergence",
  "tests/unix_background_lifecycle_acceptance.rs::kill_server_bounds_slow_session_closed_hook_tree_product_divergence",
  "tests/unix_background_lifecycle_acceptance.rs::kill_server_joins_background_session_closed_hook_trees_product_divergence",
]
limitations = [
  "Unix descendants that call setsid() and leave the tracked process group may survive shutdown",
  "RMUX drains background session-closed hooks within its bounded shutdown window; work still queued when that window expires is cancelled",
]

''')

# One more appended entry reverses exactly to the v5 ledger, with no new credit.
APPENDED_ENTRY_LEDGER_BINDING = {
    **EDITED_ENTRY_LEDGER_BINDING,
    "policy_id": "divergences.appended-entry-delta.v6",
    "current_sha256": "f65078f7d2ae978eb1772b811397bf0cc279daad313d0b8fd01ba38ee4764f26",
    "integrated_changes": EDITED_ENTRY_LEDGER_BINDING["integrated_changes"] + [{
        "commit": "7f6febd388e9e6b399d4ceccb9330ac207d799ea",
        "sha256": "f65078f7d2ae978eb1772b811397bf0cc279daad313d0b8fd01ba38ee4764f26",
    }],
    "added_ids": ["C-D93", "C-D94", "C-D95"],
}
ADDED_C_D95 = '''
[[entry]]
id = "C-D95"
summary = "IL outside the scroll region clears every inserted row"
inventory = "bounded"
evidence = ["crates/rmux-core/src/grid/scroll.rs", "crates/rmux-core/src/screen/writer.rs"]
tests = ["crates/rmux-core/src/screen/line_edit_outside_region_tests.rs::insert_line_outside_region_clears_every_inserted_row_product_divergence"]
limitations = ["With the cursor outside the region, tmux 3.7b grid_view_insert_lines only moves the surviving rows and never clears the rows they vacate: CSI 4;1H CSI 2L in a 6-row pane with region 1;3 leaves L1 L2 L3 <blank> L5 L4, and a count reaching the bottom (CSI 5;1H CSI 2L or CSI 9L) changes nothing; RMUX clears every inserted row, as its DL and the in-region IL of tmux do", "A count of 1 and every DL outside the region move rows and wrap flags as in tmux 3.7b, and nothing enters history"]
'''

# One more appended entry reverses exactly to the v6 ledger, with no new credit.
ADDED_MACOS_ENTRY_LEDGER_BINDING = {
    **APPENDED_ENTRY_LEDGER_BINDING,
    "policy_id": "divergences.added-entry-delta.v7",
    "current_sha256": "2fe0611a5721c3bcb612ce87827d8f531ceaacc89f33b35cf32acde8a557fb56",
    "integrated_changes": APPENDED_ENTRY_LEDGER_BINDING["integrated_changes"] + [{
        "commit": "ff064c20ef654e85cbbb93d299ceed891f4a811c",
        "sha256": "2fe0611a5721c3bcb612ce87827d8f531ceaacc89f33b35cf32acde8a557fb56",
    }],
    "added_ids": ["C-D93", "C-D94", "C-D95", "C-D96"],
}
ADDED_C_D96 = r'''
[[entry]]
id = "C-D96"
summary = "On macOS, m: reads a backslash inside a bracket list the glibc way, not the way of the Darwin libc"
inventory = "bounded"
evidence = ["crates/rmux-core/src/formats/tests/glob_star_escape.rs::a_list_reads_its_escapes_up_to_its_closing_bracket_on_every_host"]
limitations = ["tmux 3.7b hands m: to the fnmatch(3) of its libc, so four lists answer otherwise on macOS than on Linux: [7\\]] and [[:digit:]\\]] against 7 give 0, [a-c\\] against b gives 1 and [?\\--[:digit:] against 7 gives 0 on macOS; RMUX gives the Linux answers 1, 1, 0 and 1 on every host", "The rangematch of the Darwin libc honours a backslash of a list only until a member matches, then skips to the first ] without it, and refuses a range that ends on [: ; this is a libc defect, not a tmux reading, and copying it would need a platform selection in rmux-core"]
'''

# One more appended entry reverses exactly to the v7 ledger, with no new credit.
OCTAL_ENTRY_LEDGER_BINDING = {
    **ADDED_MACOS_ENTRY_LEDGER_BINDING,
    "policy_id": "divergences.appended-entry-delta.v8",
    "current_sha256": "dc483eb157f74f7dfcb817a6672aa50a8f7190b850af1d59cbd72cba805c7820",
    "integrated_changes": ADDED_MACOS_ENTRY_LEDGER_BINDING["integrated_changes"] + [{
        "commit": "d4aad8296c917f00c12c7a31c91f6f1572b4d334",
        "sha256": "dc483eb157f74f7dfcb817a6672aa50a8f7190b850af1d59cbd72cba805c7820",
    }],
    "added_ids": ["C-D93", "C-D94", "C-D95", "C-D96", "C-D97"],
}
ADDED_C_D97 = r'''
[[entry]]
id = "C-D97"
summary = "Command-language bytes that cannot form UTF-8 become U+FFFD"
inventory = "bounded"
evidence = ["crates/rmux-core/src/command_parser/lexer/word.rs", "crates/rmux-server/src/handler/scripting_support/source_files.rs", "crates/rmux-server/src/config_paths.rs"]
tests = ["crates/rmux-core/src/command_parser/tests.rs::octal_escapes_that_cannot_form_utf8_become_replacement_characters_product_divergence"]
limitations = ["tmux 3.7b appends the byte of a \\ooo escape and keeps a byte that cannot form UTF-8 raw: set -g @e \"\\377\", \"\\303x\" and \"a\\251b\" show ff, c3 78 and 61 a9 62, and send-keys \"\\377\" a types ff 61; RMUX arguments, options and requests are UTF-8 strings, so each maximal invalid subpart becomes U+FFFD: ef bf bd, ef bf bd 78, 61 ef bf bd 62 and ef bf bd 61", "A raw invalid byte of a sourced file becomes U+FFFD the same way through String::from_utf8_lossy: \"a<a9>b\" shows 61 ef bf bd 62 where tmux 3.7b keeps 61 a9 62; octal escapes that form UTF-8, split by a quote toggle or a line continuation included, give the bytes of tmux 3.7b"]
'''

EVIDENCE_RELOCATIONS = (
    ("crates/rmux-client/src/attach_windows/input.rs",
     "crates/rmux-client/src/attach/windows/input.rs"),
    ("crates/rmux-server/src/handler_client_runtime.rs",
     "crates/rmux-server/src/handler/client_runtime_support.rs"),
    ("crates/rmux-server/src/handler_pane/attached_input/bracketed_paste.rs",
     "crates/rmux-server/src/handler/pane_support/pane_attached_input/bracketed_paste.rs"),
    ("crates/rmux-server/src/handler_pane/attached_input/terminal_response.rs",
     "crates/rmux-server/src/handler/pane_support/pane_attached_input/terminal_response.rs"),
)

HISTORICAL_C_D32 = '''[[entry]]
id = "C-D32"
summary = "FNP1 new-pane/newp supports explicit target and positive integer geometry only; zero/percent/default geometry, Empty/key and detached CLI batches are rejected. Creation is frontmost even detached; RMUX reselect raises an active float. Layout dumps remain tiled-only."
inventory = "bounded"
evidence = ["crates/rmux-core/src/command_inventory/signatures.rs", "crates/rmux-server/src/handler_scripting_tests/fnp1.rs"]
deferred = ["floating geometry defaults/percentages", "floating Empty/key", "floating layout serialization"]
'''
CURRENT_C_D32 = '''[[entry]]
id = "C-D32"
summary = "new-pane/newp refuses non-positive dimensions and cascade placements beyond the window edge; creation is frontmost even detached and reselect raises an active float. Named layouts preserve floating membership, whereas tmux 3.7b tiles floats when rebuilding a multi-tile layout. Layout dumps remain tiled-only."
inventory = "bounded"
evidence = ["crates/rmux-core/src/command_inventory/signatures.rs", "crates/rmux-server/src/handler_scripting_tests/fnp1.rs", "crates/rmux-core/src/window/floating_tests.rs::named_layout_preserves_floats_and_float_resize_is_local"]
deferred = ["floating layout serialization"]
'''
ADDED_C_D93 = '''
[[entry]]
id = "C-D93"
summary = "set-pane-placement explicitly converts tiled and floating panes as an RMUX extension"
inventory = "bounded"
evidence = ["crates/rmux-core/src/command_inventory.rs", "crates/rmux-core/src/command_inventory/signatures.rs", "crates/rmux-server/src/handler_scripting_tests/fpc1.rs", "crates/rmux-server/src/handler_scripting_tests/fpc1_r1.rs"]
limitations = ["No tmux 3.7b oracle command exists for set-pane-placement; conversion preserves pane identity and synchronizes linked windows through the existing family transaction", "The -a/-h/-v/-b flags apply only to insertion into the tiled layout; split-window and swap-pane still reject floating targets"]
'''


# Exact entry replacements reverse to the previously accepted whole-file digest.
LEDGER_EDITS = (
    ('''[[entry]]
id = "C-D36"
summary = "RETIRED 2026-10-16 (D146): trailing bare format markers are consumed, matching tmux 3.7b"
inventory = "none"
retired_by = "D146"
historical_evidence = ["commit:7ad3957f2f7839dc38138210576a91a7ac47ad63", "R2-D9-MACRED032-TRAILING-HASH-ORACLE-197/oracle-independent.json"]
evidence = ["tests/reference/tmux_compat/R2-D9-MACRED032-TRAILING-HASH-ORACLE-197/contract-evidence.json", "tests/reference/tmux_compat/R2-D9-MACRED032-TRAILING-HASH-ORACLE-197/oracle-independent.json"]
tests = ["crates/rmux-core/src/formats/tests/transformations.rs::trailing_bare_hash_stops_expansion_like_tmux"]
limitations = ["Former divergence: A trailing bare format marker remains literal; explicitly retired by D146 after R2-197 measured tmux 3.7b on display-message -p and list-panes -F"]

''', '''[[entry]]
id = "C-D36"
summary = "A trailing bare format marker remains literal"
inventory = "bounded"
tests = ["crates/rmux-core/src/formats/tests/transformations.rs::trailing_bare_hash_is_literal_product_divergence"]

'''),
    ('''[[entry]]
id = "C-D70"
summary = "Main-screen resize retains bounded reflow state"
inventory = "bounded"
tests = [
  "crates/rmux-core/src/screen/tests.rs::width_resize_keeps_cursor_content_visible_when_only_empty_tail_rows_move_product_divergence",
  "crates/rmux-core/src/screen/tests.rs::zero_history_reflow_overflow_never_becomes_scrollable_history_product_divergence",
  "crates/rmux-core/src/screen/tests.rs::reflow_overflow_capacity_freezes_on_normal_output_after_growth_product_divergence",
  "crates/rmux-core/src/screen/tests.rs::reflow_output_capacity_stays_bounded_but_height_transfers_preserve_rows",
]

''', '''[[entry]]
id = "C-D70"
summary = "Main-screen resize retains bounded reflow state"
inventory = "bounded"
tests = [
  "crates/rmux-core/src/screen/tests.rs::width_resize_keeps_cursor_content_visible_when_only_empty_tail_rows_move_product_divergence",
  "crates/rmux-core/src/screen/tests.rs::zero_history_reflow_overflow_never_becomes_scrollable_history_product_divergence",
  "crates/rmux-core/src/screen/tests.rs::reflow_overflow_capacity_freezes_on_normal_output_after_growth_product_divergence",
]

'''),
    ('''[[entry]]
id = "C-D85"
summary = "Native RMUX accepts --version as an exact alias for -V without a daemon"
inventory = "bounded"
evidence = ["tests/fixtures/tmux37/accepted_product_divergences.txt"]
tests = ["tests/cli_surface.rs::native_long_version_product_divergence_matches_short_version", "tests/socket_dropin.rs::tmux_personality_keeps_long_version_rejection", "tests/cli_long_version_windows.rs::native_long_version_product_divergence_matches_short_version_windows", "tests/cli_long_version_windows.rs::tmux_personality_keeps_long_version_rejection_windows"]
limitations = ["Product extension, not oracle parity: tmux 3.7b rejects --version; RMUX's tmux personality retains its prior long-option rejection and tmux 3.4 short-version branding"]

''', '''[[entry]]
id = "C-D85"
summary = "Native RMUX accepts --version as an exact alias for -V without a daemon"
inventory = "bounded"
evidence = ["tests/fixtures/tmux37/accepted_product_divergences.txt"]
tests = ["tests/cli_surface.rs::native_long_version_product_divergence_matches_short_version", "tests/socket_dropin.rs::tmux_personality_keeps_long_version_rejection"]
limitations = ["Product extension, not oracle parity: tmux 3.7b rejects --version; RMUX's tmux personality retains its prior long-option rejection and tmux 3.4 short-version branding"]

'''),
)


def read_divergence_reference(root, reference, contract, strict_reader, *, historical=False):
    """Validate current bytes; optionally return the proven historical projection.

    Only the declared exact integrated delta is applicable. Both whole-file
    digests and each reverse delta must match. Other references stay strict.
    """
    binding = DIVERGENCE_BINDING
    if reference['path'] != binding['path']:
        return strict_reader(root, reference)
    require(reference == {'path': binding['path'], 'sha256': binding['historical_sha256']},
            'historical divergence locator altered')
    require(contract['source_base'] == binding['historical_source'],
            'historical divergence source differs')
    source = root / binding['path']
    require(source.resolve().is_relative_to(root.resolve()), 'divergence symlink escapes root')
    observed = hashlib.sha256(source.read_bytes()).hexdigest()
    if observed == binding['historical_sha256']:
        return strict_reader(root, reference)
    declaration = contract.get('divergence_reference_applicability')
    require(declaration in (binding, LEDGER_BINDING, RELOCATED_LEDGER_BINDING,
                            ADDED_ENTRY_LEDGER_BINDING, EDITED_ENTRY_LEDGER_BINDING,
                            APPENDED_ENTRY_LEDGER_BINDING,
                            ADDED_MACOS_ENTRY_LEDGER_BINDING,
                            OCTAL_ENTRY_LEDGER_BINDING),
            'unbound divergence applicability declaration')
    text = strict_reader(root, {'path': binding['path'], 'sha256': declaration['current_sha256']})
    previous = text
    if declaration == OCTAL_ENTRY_LEDGER_BINDING:
        require(previous.endswith(ADDED_C_D97), 'appended entry location differs')
        previous = previous[:-len(ADDED_C_D97)]
        require(hashlib.sha256(previous.encode()).hexdigest() ==
                ADDED_MACOS_ENTRY_LEDGER_BINDING['current_sha256'], 'appended entry delta exceeds C-D97')
    if declaration in (ADDED_MACOS_ENTRY_LEDGER_BINDING, OCTAL_ENTRY_LEDGER_BINDING):
        require(previous.endswith(ADDED_C_D96), 'appended entry location differs')
        previous = previous[:-len(ADDED_C_D96)]
        require(hashlib.sha256(previous.encode()).hexdigest() ==
                APPENDED_ENTRY_LEDGER_BINDING['current_sha256'], 'appended entry delta exceeds C-D96')
    if declaration in (APPENDED_ENTRY_LEDGER_BINDING, ADDED_MACOS_ENTRY_LEDGER_BINDING,
                       OCTAL_ENTRY_LEDGER_BINDING):
        require(previous.endswith(ADDED_C_D95), 'appended entry location differs')
        previous = previous[:-len(ADDED_C_D95)]
        require(hashlib.sha256(previous.encode()).hexdigest() ==
                EDITED_ENTRY_LEDGER_BINDING['current_sha256'], 'appended entry delta exceeds C-D95')
    if declaration in (EDITED_ENTRY_LEDGER_BINDING, APPENDED_ENTRY_LEDGER_BINDING,
                       ADDED_MACOS_ENTRY_LEDGER_BINDING,
                       OCTAL_ENTRY_LEDGER_BINDING):
        current, prior = EDITED_C_D44
        require(previous.count(current) == 1, 'edited entry location differs')
        previous = previous.replace(current, prior, 1)
        require(hashlib.sha256(previous.encode()).hexdigest() ==
                ADDED_ENTRY_LEDGER_BINDING['current_sha256'], 'edited entry delta exceeds C-D44')
    if declaration in (ADDED_ENTRY_LEDGER_BINDING, EDITED_ENTRY_LEDGER_BINDING,
                       APPENDED_ENTRY_LEDGER_BINDING,
                       ADDED_MACOS_ENTRY_LEDGER_BINDING,
                       OCTAL_ENTRY_LEDGER_BINDING):
        require(previous.endswith(ADDED_C_D94), 'added entry location differs')
        previous = previous[:-len(ADDED_C_D94)]
        require(hashlib.sha256(previous.encode()).hexdigest() ==
                RELOCATED_LEDGER_BINDING['current_sha256'], 'added entry delta exceeds C-D94')
    if declaration in (RELOCATED_LEDGER_BINDING, ADDED_ENTRY_LEDGER_BINDING,
                       EDITED_ENTRY_LEDGER_BINDING, APPENDED_ENTRY_LEDGER_BINDING,
                       ADDED_MACOS_ENTRY_LEDGER_BINDING,
                       OCTAL_ENTRY_LEDGER_BINDING):
        for old, new in EVIDENCE_RELOCATIONS:
            require(previous.count('"' + new + '"') == 1,
                    'evidence relocation locations differ')
            previous = previous.replace('"' + new + '"', '"' + old + '"', 1)
        require(hashlib.sha256(previous.encode()).hexdigest() == LEDGER_BINDING['current_sha256'],
                'evidence relocation exceeds C-D51, C-D52, C-D55 and C-D92')
    if declaration in (LEDGER_BINDING, RELOCATED_LEDGER_BINDING, ADDED_ENTRY_LEDGER_BINDING,
                       EDITED_ENTRY_LEDGER_BINDING, APPENDED_ENTRY_LEDGER_BINDING,
                       ADDED_MACOS_ENTRY_LEDGER_BINDING,
                       OCTAL_ENTRY_LEDGER_BINDING):
        for current, prior in LEDGER_EDITS:
            require(previous.count(current) == 1, 'integrated ledger delta locations differ')
            previous = previous.replace(current, prior, 1)
        require(hashlib.sha256(previous.encode()).hexdigest() == binding['current_sha256'],
                'integrated ledger delta exceeds C-D36, C-D70 and C-D85')
    require(previous.count(CURRENT_C_D32) == 1 and previous.endswith(ADDED_C_D93),
            'divergence delta locations differ')
    previous = previous[:-len(ADDED_C_D93)].replace(CURRENT_C_D32, HISTORICAL_C_D32, 1)
    require(hashlib.sha256(previous.encode()).hexdigest() == binding['historical_sha256'],
            'divergence delta exceeds C-D32 and C-D93')
    return previous if historical else text
