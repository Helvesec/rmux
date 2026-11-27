#!/usr/bin/env python3
"""Read a Windows cutoff contribution. No process launch or OS instrumentation.

The existing qualification completion checker owns sample accounting. Native
API facts have a separate scope: CLI timings and standalone API probes cannot
be attributed to product calls. Passing this pure reader is not native proof;
independent review must qualify the referenced observer and its exact bytes.
"""
from __future__ import annotations
import argparse
import hashlib
import json
from pathlib import Path
import qualification

KINDS = {"inbox", "companion"}
LIFETIME_FIELDS = ("api_family", "lifetime_id", "creator_operation_id", "hpcon")

def require(condition, message):
    if not condition:
        raise ValueError(message)

def integer(value, minimum=0):
    return type(value) is int and value >= minimum

def pin(value):
    return (isinstance(value, dict) and isinstance(value.get("path"), str)
            and bool(value["path"]) and integer(value.get("bytes"), 1)
            and isinstance(value.get("sha256"), str) and len(value["sha256"]) == 64
            and all(c in "0123456789abcdef" for c in value["sha256"]))

def consume(contract, data):
    errors = []
    try:
        schema = contract["schema"]
        require(type(schema) is int and schema in (1, 2) and type(data["schema"]) is int
                and data["schema"] == schema, "schema")
        require(data["scope"] == "product-conpty-calls", "CLI or standalone API scope cannot replace product calls")
        require(data["source"] == contract["source"], "source differs")
        require(data["contract_sha256"] == qualification.digest(contract), "contract changed")
        require(all(pin(data[k]) for k in ("binary", "observer", "raw_evidence", "build_provenance")), "missing evidence pins")
        require(data["binary"] == contract["binary"] and data["build_provenance"] == contract["build_provenance"], "binary/build provenance differs")
        require(data["observer_qualification"] == contract["observer_qualification"]
                and pin(data["observer_qualification"]), "unqualified observation surface")
        cases = data["cases"]
        expected = contract["expected"]
        require([x["case_id"] for x in cases] == [x["case_id"] for x in contract["cases"]], "missing, duplicate or reordered cases")
        errors.extend(qualification.completion_errors(expected, data["attempts"], data["operations"], data["notes"]))
        require(data["complete"] is True and data["flushed"] is True, "incomplete finalization")
        require(type(data["dropped"]) is int and type(data["write_errors"]) is int
                and data["dropped"] == data["write_errors"] == 0 and data["cap_hit"] is False, "capture loss")
        require(integer(data["producer_birth_filetime"], 1) and integer(data["qpc_frequency"], 1), "invalid native clock/producer")
        require(integer(data["window_start"]) and integer(data["window_end"], data["window_start"] + 1), "invalid capture window")
        operation_ids = set()
        total = 0
        kinds = set()
        pty_ids = set()
        for case, planned in zip(cases, contract["cases"]):
            require(case["kind"] == planned["kind"] and case["kind"] in KINDS, "wrong API case")
            if "branch_scope" in planned:
                require(planned["branch_scope"] in ("immediate", "deferred")
                        and case.get("branch_scope") == planned["branch_scope"], "missing or wrong observed branch")
            else:
                require("branch_scope" not in case, "unplanned branch claim")
            require(case["identity_before"] == case["identity_after"]
                    and all(integer(case["identity_before"][k]) for k in ("session_id", "window_id", "pane_id"))
                    and all(integer(case["identity_before"][k], 1) for k in ("child_pid", "birth_filetime")), "unstable identity")
            require(case["child_same_identity_alive_after_cleanup"] is False, "child cleanup unknown/live")
            require(case["geometry_before"] == planned["geometry_before"]
                    and case["geometry_after"] == planned["geometry_after"], "logical geometry mismatch")
            if schema == 2:
                require(integer(case["pty_id"]) and case["pty_id"] not in pty_ids,
                        "duplicate or invalid PTY case")
                pty_ids.add(case["pty_id"])
            calls = case["calls"]
            require(bool(calls), "missing API calls")
            require(all(integer(x["sequence"], 1) for x in calls)
                    and [x["sequence"] for x in calls] == list(range(1, len(calls)+1)), "call sequence/loss")
            successful = set()
            previous_end = None
            created_end = None
            creator_family = None
            active = None
            lifetimes = set()
            for call in calls:
                identity = (case["case_id"], call["operation_id"])
                require(integer(call["operation_id"]) and identity not in operation_ids, "invalid or duplicate API operation")
                operation_ids.add(identity)
                require(call["scope"] == "product-conpty-call" and call["case_id"] == case["case_id"], "unjoined call")
                require(call["pty_id"] == case["pty_id"] and integer(case["pty_id"]) and integer(call["pty_id"]), "wrong PTY")
                require(call["begin_dimensions"] == call["end_dimensions"], "pair dimensions differ")
                require(call["api"] in ("create", "resize"), "unknown API")
                require(isinstance(call["begin_dimensions"], list) and len(call["begin_dimensions"]) == 2
                        and all(type(v) is int and -32768 <= v < 32768 for v in call["begin_dimensions"]), "invalid COORD observation")
                if call["api"] == "create":
                    require(integer(call["flags"]) and call["flags"] <= 0xffffffff, "missing actual create flags")
                require(integer(call["begin_qpc"]) and integer(call["end_qpc"])
                        and data["window_start"] <= call["begin_qpc"] < data["window_end"]
                        and data["window_start"] <= call["end_qpc"] < data["window_end"]
                        and call["end_qpc"] >= call["begin_qpc"] - 1, "invalid per-call QPC")
                require(integer(call["qpc_frequency"], 1) and integer(call["producer_birth_filetime"], 1)
                        and call["qpc_frequency"] == data["qpc_frequency"]
                        and call["producer_birth_filetime"] == data["producer_birth_filetime"], "clock or producer changed")
                # Calls on this owned PTY are serialized. Keep the historical
                # one-tick uncertainty; do not order unrelated cases/PTYS.
                if previous_end is not None:
                    require(call["begin_qpc"] >= previous_end - 1, "same-PTY attempt chronology differs")
                if call["api"] == "resize":
                    require(created_end is not None and call["begin_qpc"] >= created_end - 1,
                            "resize precedes successful same-PTY creation")
                previous_end = call["end_qpc"]
                module = call["module"]
                require(module["kind"] in KINDS and module["provenance_available"] is True
                        and pin(module) and isinstance(module["file_version"], str) and bool(module["file_version"]), "module provenance missing")
                require(type(call["hresult"]) is int and -(2**31) <= call["hresult"] < 2**31, "HRESULT not observed")
                family = module["kind"]
                if schema == 2:
                    require(call["api_family"] in KINDS and call["api_family"] == family,
                            "API family differs from module attribution")
                    lineage = (call["lifetime_id"], call["creator_operation_id"], call["hpcon"])
                    if call["api"] == "create" and call["hresult"] != 0:
                        require(lineage == (None, None, None), "failed create claims a lifetime")
                    else:
                        require(integer(lineage[0]) and integer(lineage[1]) and integer(lineage[2], 1),
                                "missing or invalid native lifetime")
                    if call["api"] == "resize":
                        require(lineage == active, "resize differs from active creator/lifetime/handle")
                else:
                    require(not any(key in call for key in LIFETIME_FIELDS),
                            "lifetime fields require schema2")
                if call["api"] == "resize":
                    # The API selector belongs to OwnedHpcon. Forwarded
                    # function modules may have different paths and hashes.
                    require(family == creator_family, "resize API family differs from successful creator")
                if call["hresult"] == 0:
                    successful.add(call["api"])
                    if call["api"] == "create":
                        if schema == 2:
                            require(lineage[0] not in lifetimes and lineage[1] == call["operation_id"],
                                    "reused lifetime or wrong creator operation")
                            lifetimes.add(lineage[0])
                            active = lineage
                        else:
                            require(created_end is None, "recreation requires schema2 lifetime evidence")
                        created_end = call["end_qpc"]
                        creator_family = family
                kinds.add(family)
                total += 1
            require(successful == {"create", "resize"}, "no successful native create/resize")
        require(kinds == KINDS, "inbox/companion population incomplete")
        require(type(data["attempted_calls"]) is int and type(data["written_calls"]) is int
                and data["attempted_calls"] == data["written_calls"] == total, "native call cardinality")
    except (KeyError, TypeError, ValueError, IndexError, AttributeError) as error:
        errors.append(str(error))
    return {"status": "FAIL" if errors else "PASS_RECORD_CONSISTENCY_ONLY",
            "errors": errors, "native_proof_inferred": False,
            "requires_independent_observer_qualification": True}

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--contract", required=True, type=Path)
    parser.add_argument("--evidence", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    data = json.loads(args.evidence.read_text())
    result = consume(json.loads(args.contract.read_text()), data)
    if not result["errors"]:
        pins = [data[key] for key in ("binary", "observer", "raw_evidence", "build_provenance", "observer_qualification")]
        pins += [call["module"] for case in data["cases"] for call in case["calls"]]
        for item in pins:
            try:
                raw = Path(item["path"]).read_bytes()
                require(len(raw) == item["bytes"] and hashlib.sha256(raw).hexdigest() == item["sha256"], "evidence bytes differ: " + item["path"])
            except (OSError, ValueError) as error:
                result["errors"].append(str(error))
        if result["errors"]:
            result["status"] = "FAIL"
    result["inputs"] = [{"path": str(p), "bytes": p.stat().st_size,
                         "sha256": hashlib.sha256(p.read_bytes()).hexdigest()}
                        for p in (args.contract, args.evidence)]
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    return int(bool(result["errors"]))

if __name__ == "__main__":
    raise SystemExit(main())
