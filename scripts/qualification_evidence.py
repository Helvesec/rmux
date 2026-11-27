"""Validate retained runner evidence against the independent candidate recipe."""
from pathlib import Path, PurePosixPath, PureWindowsPath
import json
import re

import candidate_identity as identity
import qualification_final_smokes as final_smokes
from qualification_campaign import (attempt_journal, attempt_key, check_output, digest, file_hash,
                                    require, test_population, validate_manifest)
from qualification_recipe import default_manifest, expected_matrix


def check_portable_identity(executable, context, head, os_name):
    """Check collector bindings; raw native bytes were verified before each PASS."""
    path_type = PureWindowsPath if os_name == "windows11" else PurePosixPath
    source, target, path = (path_type(value) for value in
                            (context["source"], context["target"], executable["path"]))
    base = source / ".rmux-audit" / (head if os_name == "windows11" else "candidate-target")
    if os_name != "windows11":
        base = base / head / os_name
    require(source.is_absolute() and path.is_absolute() and target.is_absolute() and
            ".." not in path.parts and ".." not in target.parts and
            target.parent == base and path.is_relative_to(target) and
            re.fullmatch("[a-z2-7]{26}" if os_name == "windows11" else "[0-9a-f]{32}", target.name),
            "foreign D41 target/path")
    require(executable["commit"] == head and executable["source"] == context["source"] and
            executable["target"] == context["target"] and
            executable["os"] == ("windows" if os_name == "windows11" else os_name) and
            re.fullmatch("[0-9a-f]{64}", executable["sha256"]), "false D41 identity")


def check_campaign(root, directories, *, synthetic=False, final_artifact_manifest=None):
    root = Path(root).resolve()
    require(directories, "no campaign evidence")
    seen, runs, executions, challenges, manifest = set(), set(), set(), set(), None
    final_cells = 0
    for directory in map(Path, directories):
        read = lambda name: json.loads((directory / (name + ".json")).read_text())
        current = read("manifest")
        validate_manifest(current)
        final_smokes.require_current_recipe(current, synthetic)
        context, summary, receipts = read("context"), read("summary"), read("receipts")
        require(context["synthetic"] is synthetic and summary["synthetic"] is synthetic,
                "synthetic evidence is not native proof")
        if manifest is None:
            manifest = current
            if not synthetic:
                identity.source_identity(root, current["source_sha"])
                require(current == default_manifest(root, current["source_sha"], current["qualification_id"],
                                                   final_artifact_manifest=final_artifact_manifest),
                        "manifest differs from complete candidate recipe")
        require(current == manifest, "mixed campaign manifests")
        expected = expected_matrix(current)
        require(read("expected") == expected, "expected matrix differs")
        require(context["manifest_sha256"] == summary["manifest_sha256"] == digest(current), "manifest binding")
        if current["schema"] == 2:
            require(file_hash(directory / "final-manifest.json") == current["final_artifacts"]["manifest_sha256"] and
                    file_hash(directory / "final-manifest.json.sig") == current["final_artifacts"]["signature_sha256"],
                    "final artifact retained manifest bytes differ")
        require(context["run_id"] == summary["run_id"] and context["run_id"] not in runs, "run reused")
        runs.add(context["run_id"])
        attempts = read("attempts")
        require(attempts and len({a["cell_id"] for a in attempts}) == len(attempts), "missing/duplicate attempt journal")
        journal = attempt_journal(root, synthetic, current)
        for attempt in attempts:
            cells = [c for c in current["cells"] if c["id"] == attempt["cell_id"] and
                     c["native_os"] == attempt["native_os"]]
            require(len(cells) == 1, "shared attempt journal cell differs")
            path = journal / (digest(attempt_key(current, cells[0])) + ".json")
            require(path.is_file() and json.loads(path.read_text()) == attempt,
                    "shared attempt journal missing or differs")
            require(attempt["source_sha"] == current["source_sha"] and
                    attempt["qualification_id"] == current["qualification_id"] and
                    attempt["run_id"] == context["run_id"] and attempt["manifest_sha256"] == digest(current) and
                    type(attempt["attempt"]) is int and attempt["attempt"] == 1, "attempt journal differs")
        local = set()
        for index, receipt in enumerate(receipts):
            require(receipt == read(str(index) + ".receipt"), "receipt copies differ")
            ticket = read(str(index) + ".challenge")
            key = (receipt["cell_id"], receipt["scenario_id"])
            require(key not in seen and {"cell_id": key[0], "scenario_id": key[1]} in expected,
                    "cell duplicated or absent from manifest")
            seen.add(key)
            local.add(key)
            matched_attempts = [a for a in attempts if a["cell_id"] == key[0]]
            require(len(matched_attempts) == 1, "missing cell attempt journal")
            attempt = matched_attempts[0]
            require(attempt["issued_ns"] <= receipt["issued_ns"] and
                    attempt["native_os"] == receipt["native_os"] and attempt["source"] == receipt["source"],
                    "attempt binding differs")
            require(receipt["status"] == "PASS" and receipt["executed"] is True and
                    receipt["skip"] is False and type(receipt["attempt"]) is int and
                    receipt["attempt"] == 1, "failed/skip/retry/nonexecution receipt")
            require(receipt["synthetic"] is synthetic and receipt["source_sha"] == current["source_sha"] and
                    receipt["manifest_sha256"] == digest(current) and receipt["run_id"] == context["run_id"],
                    "receipt identity differs")
            for field in ("challenge_id", "execution_id", "issued_ns", "cell_id", "scenario_id",
                          "source_sha", "run_id", "attempt", "synthetic", "manifest_sha256"):
                require(ticket[field] == receipt[field], "challenge binding differs")
            require(receipt["challenge_id"] not in challenges and receipt["execution_id"] not in executions,
                    "execution/challenge reused")
            challenges.add(receipt["challenge_id"])
            executions.add(receipt["execution_id"])
            require(all(re.fullmatch("[0-9a-f]{64}", receipt[field]) for field in ("challenge_id", "execution_id")),
                    "invalid execution/challenge identity")
            require(all(type(receipt[field]) is int for field in ("issued_ns", "started_ns", "ended_ns")),
                    "invalid execution timestamps")
            require(0 < receipt["issued_ns"] < receipt["started_ns"] <= receipt["ended_ns"], "execution chronology")
            require(type(receipt["exit_code"]) is int and receipt["exit_code"] == 0, "command failure")
            scenario = next(s for s in current["scenarios"] if s["id"] == key[1])
            require(receipt["command"] == scenario["command"], "command differs")
            os_name = next(c["native_os"] for c in current["cells"] if c["id"] == key[0])
            arguments = [arg.replace("{platform}", "windows" if os_name == "windows11" else os_name)
                         for arg in scenario["command"]]
            if scenario["id"] == final_smokes.SCENARIO:
                require(isinstance(receipt["argv"], list) and len(receipt["argv"]) == 7,
                        "final artifact executed command differs")
                path_type = PureWindowsPath if os_name == "windows11" else PurePosixPath
                native_manifest = path_type(receipt["argv"][4])
                require(native_manifest.is_absolute() and ".." not in native_manifest.parts,
                        "final artifact manifest path differs")
                output = path_type(attempt["evidence"]) / (str(index) + ".final-smokes")
                replacements = {"{final_manifest}": str(native_manifest), "{final_output}": str(output)}
                arguments = [replacements.get(arg, arg) for arg in arguments]
            require(receipt["argv"][1:] == arguments[1:] and
                    re.fullmatch("[0-9a-f]{64}", receipt["tool_sha256"]), "executed command differs")
            out, err = directory / (str(index) + ".stdout"), directory / (str(index) + ".stderr")
            require(file_hash(out) == receipt["stdout_sha256"] and file_hash(err) == receipt["stderr_sha256"],
                    "output bytes differ")
            population = None
            if scenario.get("tests"):
                lo, le = directory / (str(index) + ".list.stdout"), directory / (str(index) + ".list.stderr")
                require(file_hash(lo) == receipt["list_stdout_sha256"] and
                        file_hash(le) == receipt["list_stderr_sha256"], "test listing bytes differ")
                population = test_population(lo.read_text(errors="replace"))
                require(dict(population) == receipt["population"], "test population differs")
            check_output(out.read_text(errors="replace") + "\n" + err.read_text(errors="replace"), scenario, population)
            if scenario["id"] == final_smokes.SCENARIO:
                final_cells += final_smokes.check_output(current, receipt,
                    directory.absolute() / (str(index) + ".final-smokes"), final_artifact_manifest)
            if not synthetic:
                require(receipt["source"] == context["source"], "D41 source differs")
                os_name = next(c["native_os"] for c in current["cells"] if c["id"] == key[0])
                require(receipt["native_os"] == os_name, "native OS relabelled")
                require(not scenario.get("tests") or receipt["executables"], "missing D41 launch receipts")
                launches = directory / (str(index) + ".launches.json")
                require(file_hash(launches) == receipt["launches_sha256"], "D41 launch bytes differ")
                records = json.loads(launches.read_text())
                require(records == receipt["identity_launches"] and
                        [e for r in records for e in r["executables"]] == receipt["executables"],
                        "D41 launch binding differs")
                for executable in receipt["executables"]:
                    check_portable_identity(executable, context, current["source_sha"], os_name)
                oracle = context["oracle"]
                require(oracle is None if os_name == "windows11" else
                        oracle["version"] == "tmux 3.7b" and
                        re.fullmatch("[0-9a-f]{64}", oracle["sha256"]) and
                        ((oracle["reference_sha256"] is None and oracle["source_sha"] is None) or
                         (re.fullmatch("[0-9a-f]{64}", oracle["reference_sha256"]) and
                          oracle["source_sha"] == "e802909de06012a4df6209d55e86487c56223163")),
                        "oracle identity differs")
        pending = [row for row in expected if (row["cell_id"], row["scenario_id"]) not in local]
        require(summary["pending"] == pending and summary["expected"] == len(expected) and
                summary["executed"] == len(receipts) and
                summary["status"] == ("PASS_FIXTURE" if synthetic else "PASS_CELL"), "summary differs")
    require(seen == {(r["cell_id"], r["scenario_id"]) for r in expected}, "missing native cell/scenario receipt")
    require(manifest["schema"] == 1 or final_cells == 13, "missing final artifact smoke cells (4+4+5 required)")
    result = {"status": "PASS_FIXTURE" if synthetic else "PASS_QUALIFICATION_ACCOUNTING",
            "qualification_id": manifest["qualification_id"], "source_sha": manifest["source_sha"],
            "executions": len(seen), "synthetic": synthetic,
            "native_authenticity_or_product_verdict": False}
    if manifest["schema"] == 2:
        result["final_smoke_cells"] = final_cells
    return result
