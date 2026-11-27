"""Execute the qualification matrix once; retain failures and every pending cell."""
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import subprocess
import sys
import time

import candidate_identity as identity
import qualification_final_smokes as final_smokes
from qualification_recipe import applicable, default_manifest, expected_matrix, launch_records


# Provisioned by the campaign owner, never by a runner or from its environment.
# Every host's entry must address the same shared directory (native mount aliases
# are explicit). A missing registration is an error, not first-use enrollment.
AUTHORITY_FILE = Path("C:/ProgramData/rmux/qualification-authority.json" if sys.platform == "win32"
                      else "/etc/rmux/qualification-authority.json")


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":"),
                                     allow_nan=False).encode()).hexdigest()


def file_hash(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def write(path, value):
    # Never replace a prior execution or verdict, including an interrupted one.
    with path.open("x") as stream:
        json.dump(value, stream, sort_keys=True)
        stream.write("\n")


def validate_manifest(manifest):
    require(type(manifest["schema"]) is int and manifest["schema"] in {1, 2} and
            re.fullmatch("[0-9a-f]{40}", manifest["source_sha"]),
            "manifest identity")
    require(manifest["qualification_id"] in {"Q0", "Q1", "Q2"}, "qualification identity")
    for field in ("scenarios", "cells"):
        rows = manifest[field]
        require(isinstance(rows, list) and rows, "empty " + field)
        ids = [r["id"] for r in rows]
        require(all(isinstance(i, str) and i for i in ids) and len(set(ids)) == len(ids),
                "missing/duplicate " + field)
    for scenario in manifest["scenarios"]:
        command = scenario["command"]
        require(isinstance(command, list) and command and
                all(isinstance(a, str) and a for a in command), "invalid command")
        platforms = scenario.get("native_os", ["linux", "macos", "windows11"])
        require(isinstance(platforms, list) and platforms and
                len(set(platforms)) == len(platforms) and
                all(p in {"linux", "macos", "windows11"} for p in platforms), "invalid OS applicability")
    require(all(c["native_os"] in {"linux", "macos", "windows11"} for c in manifest["cells"]),
            "unsupported native cell")
    if manifest["schema"] == 2:
        final_smokes.validate_recipe(manifest)


def native_os():
    if sys.platform == "win32":
        require(sys.getwindowsversion().build >= 22000, "Windows 11 authority required")
        return "windows11"
    return identity.host_os()


def oracle_record(env):
    chosen = env.get("RMUX_FROZEN_TMUX")
    require(chosen and Path(chosen).is_absolute(), "absolute RMUX_FROZEN_TMUX required")
    path = Path(chosen).resolve(strict=True)
    other = env.get("RMUX_TMUX_ORACLE")
    require(not other or (Path(other).is_absolute() and Path(other).resolve() == path),
            "tmux selectors diverge")
    env.update(RMUX_FROZEN_TMUX=str(path), RMUX_TMUX_ORACLE=str(path))
    version = subprocess.check_output([str(path), "-V"], text=True).strip()
    require(version == "tmux 3.7b", "tmux 3.7b required")
    reference = path.with_name("tmux.reference")
    record = {"path": str(path), "version": version, "sha256": file_hash(path),
              "reference_sha256": None, "source_sha": None}
    if not reference.exists():
        # Installed native authorities can have no build sidecar. Retain the
        # measured version/bytes without inventing source provenance.
        return record
    pairs = re.findall(r'^([a-z_0-9]+): "([^"\n]+)"$', reference.read_text(), re.M)
    require(len(pairs) == len({key for key, _ in pairs}), "ambiguous tmux reference")
    fields = dict(pairs)
    require(fields.get("source_sha") == "e802909de06012a4df6209d55e86487c56223163",
            "tmux source identity differs")
    require(fields.get("binary_sha256") == file_hash(path), "tmux binary identity differs")
    require(fields.get("version") == version and fields.get("source_tag") == "3.7b" and
            fields.get("source_tarball_sha256") ==
            "87f2e99e3b685973f2ca002ffd6ed7e51a5744f7009daae5a15670b6d532db96", "tmux reference differs")
    return dict(record, reference_sha256=file_hash(reference), source_sha=fields["source_sha"])


def test_population(text):
    return Counter(re.findall(r"^(.+): test$", text, re.M))


def check_output(text, scenario, expected=None):
    require(not re.search(r"(?im)^\s*(?:skip(?:ped|ping)?\b)|\bskipped=[1-9]|"
                          r"\b[1-9][0-9]* ignored\b", text), "skip is not qualification PASS")
    if scenario.get("tests"):
        results = re.findall(r"test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;", text)
        require(results and sum(int(r[0]) for r in results) > 0, "empty test population")
        require(all(int(r[1]) == int(r[2]) == 0 for r in results), "failed/ignored test")
        actual = Counter(re.findall(r"^test (.+) \.\.\. ok$", text, re.M))
        require(expected and actual == expected, "listed test population was not executed exactly")


def resolve_command(root, command, os_name):
    argv = [a.replace("{platform}", "windows" if os_name == "windows11" else os_name)
            for a in command]
    program = sys.executable if argv[0] == "python" else shutil.which(argv[0])
    require(program is not None, "command tool unavailable: " + argv[0])
    Path(program).resolve(strict=True)
    # rustup proxies dispatch on argv[0]; resolving cargo to rustup changes the command.
    argv[0] = str(Path(program).absolute())
    return argv


def listing_command(argv):
    # Keep the libtest filters: the listed population must be the executed one.
    require("--" in argv, "test command has no libtest arguments")
    return argv + ["--list"]


def run_command(argv, root, env, stdout, stderr):
    with stdout.open("xb") as out, stderr.open("xb") as err:
        return subprocess.run(argv, cwd=root, env=env, stdout=out, stderr=err).returncode


def registered_journal(manifest):
    """Read the system trust anchor for source_sha/Qn, independently of clones.

    Registry schema 1: {"schema": 1, "campaigns": [{"source_sha": "...",
    "qualification_id": "Q0", "paths": {"macos": "...", "linux": "...",
    "windows11": "..."}}]}. The paths are mounts of ONE shared journal.
    This separate deployment registry does not change any evidence wire schema.
    """
    try:
        registry = json.loads(AUTHORITY_FILE.read_text())
        require(type(registry["schema"]) is int and registry["schema"] == 1,
                "shared qualification journal registry schema")
        entries = registry["campaigns"]
        require(isinstance(entries, list), "shared qualification journal registry campaigns")
        matches = [entry for entry in entries if entry["source_sha"] == manifest["source_sha"] and
                   entry["qualification_id"] == manifest["qualification_id"]]
        require(len(matches) == 1, "shared qualification journal authority missing/ambiguous")
        path = Path(matches[0]["paths"][native_os()])
        require(path.is_absolute(), "shared qualification journal authority must be absolute")
        return path.resolve(strict=True)
    except (OSError, KeyError, TypeError) as error:
        raise ValueError("shared qualification journal authority unavailable") from error


def attempt_journal(root, synthetic, manifest=None):
    if synthetic:
        directory = root / ".rmux-audit" / "qualification-attempts"
        directory.mkdir(parents=True, exist_ok=True)
        return directory
    # The environment asserts the registered mount; it cannot select authority.
    chosen = os.environ.get("RMUX_QUALIFICATION_JOURNAL")
    require(chosen and Path(chosen).is_absolute(), "absolute shared qualification journal required")
    directory = Path(chosen).resolve()
    require(directory.is_dir() and not directory.is_relative_to(root),
            "shared qualification journal must exist outside the checkout")
    require(manifest is not None and directory == registered_journal(manifest),
            "shared qualification journal differs from registered campaign authority")
    # A repository-local journal is not authority shared by independent clones.
    program = shutil.which("git")
    require(program is not None, "Git required to verify shared qualification journal")
    require(subprocess.run([program, "-C", str(directory), "rev-parse", "--show-toplevel"],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode != 0,
            "shared qualification journal cannot be inside a Git checkout")
    return directory


def attempt_key(manifest, cell):
    return {"source_sha": manifest["source_sha"], "qualification_id": manifest["qualification_id"],
            "cell_id": cell["id"], "native_os": cell["native_os"]}


def claim_attempts(root, manifest, cells, evidence, run_id, synthetic):
    directory = attempt_journal(root, synthetic, manifest)
    records = []
    for cell in cells:
        key = attempt_key(manifest, cell)
        record = dict(key, attempt=1, run_id=run_id, manifest_sha256=digest(manifest),
                      evidence=str(evidence), source=str(root), issued_ns=time.time_ns())
        try:
            write(directory / (digest(key) + ".json"), record)
        except FileExistsError as error:
            raise ValueError("qualification cell already attempted; retain its failure, do not retry") from error
        records.append(record)
    return records


def run_campaign(root, manifest, evidence, *, synthetic=False, final_artifact_manifest=None):
    root, evidence = Path(root).resolve(), Path(evidence).absolute()
    validate_manifest(manifest)
    final_smokes.require_current_recipe(manifest, synthetic)
    require(not evidence.exists(), "campaign already exists; retry cannot replace qualification")
    run_id = secrets.token_hex(32)
    os_name = native_os()
    cells = manifest["cells"] if synthetic else [c for c in manifest["cells"] if c["native_os"] == os_name]
    require(cells, "no native cell for this host")
    attempts = claim_attempts(root, manifest, cells, evidence, run_id, synthetic)
    evidence.mkdir(parents=True)
    write(evidence / "manifest.json", manifest)
    write(evidence / "attempts.json", attempts)
    expected = expected_matrix(manifest)
    write(evidence / "expected.json", expected)
    env = dict(os.environ, RMUX_REQUIRE_TMUX="1", RUST_MIN_STACK="8388608")
    receipt_base = {"manifest_sha256": digest(manifest), "run_id": run_id, "attempt": 1,
                    "synthetic": synthetic, "source_sha": manifest["source_sha"], "source": str(root)}
    context_error, target, oracle = None, None, None
    try:
        if not synthetic:
            identity.source_identity(root, manifest["source_sha"])
            require(manifest == default_manifest(root, manifest["source_sha"], manifest["qualification_id"],
                                                final_artifact_manifest=final_artifact_manifest),
                    "manifest differs from complete candidate recipe")
            if manifest["schema"] == 2:
                for suffix in ("", ".sig"):
                    with (evidence / ("final-manifest.json" + suffix)).open("xb") as stream:
                        stream.write(Path(str(final_artifact_manifest) + suffix).read_bytes())
            require(Path.cwd().resolve() == root, "run from the candidate checkout")
            env = dict(identity.prepare(), RMUX_REQUIRE_TMUX="1", RUST_MIN_STACK="8388608")
            target = Path(env["RMUX_IDENTITY_TARGET"])
            require(env["RMUX_IDENTITY_HEAD"] == manifest["source_sha"] and
                    Path(env["RMUX_IDENTITY_SOURCE"]).resolve() == root, "false candidate identity")
            if os_name != "windows11":
                oracle = oracle_record(env)
    except (ValueError, KeyError, TypeError, ImportError, OSError, identity.IdentityError, subprocess.SubprocessError) as error:
        context_error = str(error)
    write(evidence / "context.json", dict(receipt_base, target=str(target) if target else None, oracle=oracle))
    receipts, stopped = [], False
    for cell in cells:
        for scenario in manifest["scenarios"]:
            if not applicable(scenario, cell):
                continue
            if stopped:
                break
            index = len(receipts)
            before = set((target / "identity-launches").glob("*.json")) if target else set()
            receipt = dict(receipt_base, cell_id=cell["id"], scenario_id=scenario["id"],
                           native_os=cell["native_os"], command=scenario["command"],
                           status="FAIL", executed=False, skip=False,
                           challenge_id=secrets.token_hex(32), issued_ns=time.time_ns(),
                           execution_id=secrets.token_hex(32), executables=[])
            write(evidence / (str(index) + ".challenge.json"), receipt)
            receipt["started_ns"] = time.time_ns()
            out, err = evidence / (str(index) + ".stdout"), evidence / (str(index) + ".stderr")
            try:
                require(context_error is None, context_error)
                if not synthetic:
                    floor = {"macos": 35, "linux": 45, "windows11": 50}[os_name] * 1024**3
                    require(shutil.disk_usage(root).free >= floor, "native disk stop threshold reached")
                    with_env = dict(os.environ)
                    try:
                        os.environ.update(env)
                        identity.validate_context()
                    finally:
                        os.environ.clear()
                        os.environ.update(with_env)
                command = scenario["command"]
                final_output = evidence / (str(index) + ".final-smokes")
                if scenario["id"] == final_smokes.SCENARIO:
                    require(manifest["schema"] == 2 and not synthetic, "final artifact recipe required")
                    require(not final_output.exists(), "final artifact output already exists")
                    command = final_smokes.command_arguments(command, final_artifact_manifest, final_output)
                argv = resolve_command(root, command, cell["native_os"])
                receipt["argv"] = argv
                receipt["tool_sha256"] = file_hash(argv[0])
                population = None
                if scenario.get("tests"):
                    listing = listing_command(argv)
                    lo, le = evidence / (str(index) + ".list.stdout"), evidence / (str(index) + ".list.stderr")
                    code = run_command(listing, root, env, lo, le)
                    require(code == 0, "test listing failed")
                    population = test_population(lo.read_text(errors="replace"))
                    require(population, "test listing selected zero tests")
                    receipt["population"] = dict(population)
                    receipt["list_stdout_sha256"] = file_hash(lo)
                    receipt["list_stderr_sha256"] = file_hash(le)
                receipt["exit_code"] = run_command(argv, root, env, out, err)
                receipt["executed"] = True
                require(receipt["exit_code"] == 0, "command failed")
                check_output(out.read_text(errors="replace") + "\n" + err.read_text(errors="replace"),
                             scenario, population)
                if scenario["id"] == final_smokes.SCENARIO:
                    receipt["final_artifacts"] = final_smokes.seal_output(
                        manifest, receipt, final_output, final_artifact_manifest)
                if not synthetic:
                    identity.source_identity(root, manifest["source_sha"])
                    executables = [e for record in launch_records(target, before) for e in record["executables"]]
                    if scenario.get("tests"):
                        require(executables, "missing D41 executable launch receipts")
                    for executable in executables:
                        check_executable(executable, root, manifest["source_sha"], target, cell["native_os"])
                    receipt["executables"] = executables
                    if oracle is not None:
                        require(oracle_record(env) == oracle, "tmux oracle changed during qualification")
                receipt["status"] = "PASS"
            except (ValueError, KeyError, TypeError, ImportError, OSError, identity.IdentityError, subprocess.SubprocessError) as error:
                receipt["error"] = str(error)
                stopped = True
            finally:
                if target is not None:
                    receipt["identity_launches"] = launch_records(target, before)
                    receipt["executables"] = [e for record in receipt["identity_launches"]
                                              for e in record["executables"]]
                    launches = evidence / (str(index) + ".launches.json")
                    write(launches, receipt["identity_launches"])
                    receipt["launches_sha256"] = file_hash(launches)
                for path in (out, err):
                    if not path.exists():
                        path.write_bytes(b"")
                receipt.update(ended_ns=time.time_ns(), stdout_sha256=file_hash(out), stderr_sha256=file_hash(err))
                write(evidence / (str(index) + ".receipt.json"), receipt)
                receipts.append(receipt)
    write(evidence / "receipts.json", receipts)
    summary = {"status": "FAIL" if stopped else "PASS_FIXTURE" if synthetic else "PASS_CELL",
               "synthetic": synthetic, "expected": len(expected), "executed": sum(r["executed"] for r in receipts),
               "pending": [row for row in expected if (row["cell_id"], row["scenario_id"]) not in
                           {(r["cell_id"], r["scenario_id"]) for r in receipts}],
               "manifest_sha256": digest(manifest), "run_id": run_id}
    write(evidence / "summary.json", summary)
    return summary


def check_executable(record, root, head, target, os_name):
    path = Path(record["path"])
    require(record["commit"] == head and Path(record["source"]).resolve() == root and
            Path(record["target"]).resolve() == target and
            record["os"] == ("windows" if os_name == "windows11" else os_name), "false D41 identity")
    require(path.is_absolute() and path.resolve(strict=True).is_relative_to(target), "foreign D41 executable")
    require(file_hash(path) == record["sha256"], "D41 bytes differ")
    marker = (head.encode(), hashlib.sha256(os.fsencode(str(root))).hexdigest().encode(), record["os"].encode())
    require(set(identity.MARKER.findall(path.read_bytes())) == {marker}, "false embedded D41 identity")
