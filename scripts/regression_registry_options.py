"""Replay PASS510's existing Linux option bank and its H/U supplement."""
import copy
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

import candidate_identity as identity


ROOT = Path(__file__).resolve().parents[1]
FLAGS = ROOT / "scripts/regression_registry_options_flags.json"


def bank_modules():
    sys.path.insert(0, str(ROOT / "tests"))
    import options_bank
    import options_bank_linux
    return options_bank, options_bank_linux


def accepted_plan():
    import regression_registry as registry
    expected = json.loads(registry.DEFAULT.read_text())["receipts"]["options-flags-plan"]["sha256"]
    data = FLAGS.read_bytes()
    if hashlib.sha256(data).hexdigest() != expected:
        raise RuntimeError("accepted H/U plan changed")
    plan = json.loads(data)
    if len(plan["operations"]) != 58:
        raise RuntimeError("accepted H/U population")
    return plan


def option_names(document, template, bank):
    bank.inventory_gate(document, template)
    if document["registry_count"] != 153:
        raise RuntimeError("accepted 153-option population")
    names = ["default-shell", "status-interval"]
    names += sorted(set(entry["name"] for entry in document["options"]) - set(names))
    if len(names) != 153:
        raise RuntimeError("accepted option names")
    return names


def contract_for(template, name, registry, source, head, candidate, oracle, bank, adapter,
                 extra_artifacts=()):
    contract = copy.deepcopy(template)
    recipe = dict(schema=1, kind="per-option-v1", names=[name],
                  startup=bank.REGISTRY_STARTUP_V1, environment={"SHELL": "/bin/bash"},
                  token="backspace-input-s1096")
    contract.update(
        registry=adapter.pin(registry), source_path=str(source), native_source=head,
        native_os="linux", python=adapter.pin(Path(sys.executable).resolve()),
        default_driver=adapter.pin(ROOT / "tests/options_bank_linux.py"),
        native_binaries=dict(candidate=adapter.pin(candidate), oracle=adapter.pin(oracle)),
        default_recipe=recipe,
        default_recipe_sha256=hashlib.sha256(json.dumps(recipe, sort_keys=True).encode()).hexdigest(),
        recipe_sha256=hashlib.sha256(json.dumps(template["recipe"], sort_keys=True).encode()).hexdigest())
    paths = sorted((ROOT / "tests").glob("options_bank*.py"))
    paths += [ROOT / "tests/options_geometry_linux.py", ROOT / "tests/options_bank_contract.json",
              ROOT / "crates/rmux-core/src/options/registry/table.rs",
              ROOT / "crates/rmux-proto/src/types/options.rs", registry]
    paths += list(extra_artifacts)
    contract["artifacts"] = [adapter.pin(path) for path in paths]
    return contract


def per_option_pair(contract, directory, adapter, run, proof):
    path = directory / "contract.json"
    adapter.save(path, contract)
    oracle_observations = None
    for phase in ("oracle", "candidate"):
        if phase == "candidate":
            identity.record_launch([proof])
        run([sys.executable, "-B", ROOT / "tests/options_bank_linux.py", "per-option",
             path, phase, directory / phase], timeout=420)
        receipt = adapter.load(directory / phase / "RESULT.json")
        observations = adapter.per_option_phase(receipt, contract, phase)
        if phase == "oracle":
            oracle_observations = observations
            adapter.save(directory / "ORACLE-FROZEN.json",
                         dict(receipt=adapter.pin(directory / phase / "RESULT.json")))
        else:
            adapter.require(observations == oracle_observations, "per-option oracle/candidate mismatch")


def flags_phase(contract, phase, directory, plan, adapter, proof):
    """The retained INT270 probe, using its unchanged 58-operation plan."""
    for reference in (*contract["artifacts"], *contract["native_binaries"].values(),
                      contract["python"], contract["default_driver"]):
        adapter.authenticate(reference)
    directory.mkdir()
    runner = adapter.Run(contract, phase, directory)
    checks = []
    error = None
    try:
        if phase == "candidate":
            identity.record_launch([proof])
        runner.start_server()
        runner.start_pane()
        for step in plan["operations"]:
            adapter.require(shutil.disk_usage(ROOT).free / 2**30 > 45
                            and shutil.disk_usage("/").free / 2**30 >= 60, "disk stop")
            stdout, index = runner.command(step["role"], step["argv"], check=False)
            observed = runner.record["commands"][index]
            adapter.require(observed["exit_code"] == step["exit_code"], step["role"] + " exit")
            if "stdout" in step:
                adapter.require(stdout == step["stdout"], step["role"] + " exact output")
            if "contains" in step:
                adapter.require(step["contains"] in stdout, step["role"] + " hook inclusion")
            if "absent" in step:
                adapter.require(step["absent"] not in stdout, step["role"] + " hook exclusion")
            if "stderr_contains" in step:
                adapter.require(step["stderr_contains"] in observed["stderr"], step["role"] + " error")
            else:
                adapter.require(observed["stderr"] == "", step["role"] + " stderr")
            projected = ("".join(line + "\n" for line in stdout.splitlines()
                                 if line.startswith("after-new-window["))
                         if step["role"] in ("H-plain-list", "H-hook-list") else stdout)
            checks.append(dict(role=step["role"], argv=step["argv"],
                               exit_code=observed["exit_code"], stdout=projected))
    except Exception as caught:
        error = str(caught)
        raise
    finally:
        try:
            runner.cleanup()
        except Exception as caught:
            error = str(caught)
            raise
        finally:
            runner.record.update(checks=checks, error=error, ended=adapter.now(),
                                 executed_plan=adapter.pin(FLAGS), head=contract["native_source"],
                                 candidate_identity=proof, observer=adapter.describe(os.getpid()),
                                 status=("PASS" if error is None and len(checks) == 58
                                         and runner.record["closure"]["status"] == "PASS" else "FAIL"))
            adapter.save(directory / "RESULT.json", runner.record)
    adapter.require(runner.record["closure"]["status"] == "PASS"
                    and len(checks) == 58, "H/U population or native closure")
    return checks


def replay(target):
    if sys.platform != "linux":
        raise RuntimeError("options per-option bank requires its accepted Linux profile")
    identity.validate_context()
    source, head, active_target = identity.context()
    if source != ROOT or active_target != target:
        raise RuntimeError("option bank identity source/target differs")
    # The retained screen consumers require this dependency before native work.
    pyte = __import__("pyte")
    dependencies = sorted(Path(pyte.__file__).resolve(strict=True).parent.glob("*.py"))
    from regression_registry_checks import run
    import regression_registry as registry
    bank, adapter = bank_modules()
    plan = accepted_plan()
    audit = ROOT / ".rmux-audit"
    audit.mkdir(exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="op-", dir=audit))
    # Native Unix sockets need a short path; durable receipts stay in the worktree.
    adapter.require(len(os.fsencode(directory / "000/candidate/socket")) < 104,
                    "option bank source path is too long for native sockets")
    export = directory / "registry.json"
    command = next(item for item in registry.load_registry()
                   if item["id"] == "formats-core-options-ignored")
    oracle_table = Path(os.environ.get("RMUX_FROZEN_OPTIONS_TABLE",
                                      "/opt/rmux/reference/tmux/options-table.c")).resolve(strict=True)
    env = dict(os.environ, RMUX_FROZEN_OPTIONS_TABLE=str(oracle_table),
               RMUX_OPTIONS_BANK_OUTPUT=str(export))
    code = registry.run_commands([dict(command, argv=command["test_argv"])], ROOT, env=env)
    if code:
        raise subprocess.CalledProcessError(code, command["test_argv"])
    document = adapter.load(export)
    template = adapter.load(ROOT / "tests/options_bank_contract.json")
    names = option_names(document, template, bank)
    oracle = Path("/usr/local/bin/tmux").resolve(strict=True)
    version = run([oracle, "-V"], stdout=subprocess.PIPE, text=True).stdout.strip()
    adapter.require(version == "tmux 3.7b", "frozen oracle version")
    run(["cargo", "build", "--locked", "--offline", "--bin", "rmux"])
    selected = run([sys.executable, ROOT / "scripts/candidate_identity.py", "check",
                    target / "debug/rmux"], stdout=subprocess.PIPE, text=True)
    proof = identity.verify_built(Path(selected.stdout.strip()), source, head, target)
    built = adapter.load(proof["build"])
    candidate = Path(built["producer"] if built["kind"] == "unix-launcher" else built["path"])
    proof = identity.verify_built(candidate, source, head, target)
    contracts, rows = {}, []
    for index, name in enumerate(names):
        contract = contract_for(template, name, export, source, head, candidate, oracle, bank, adapter,
                                dependencies)
        row = directory / f"{index:03}"
        row.mkdir()
        per_option_pair(contract, row, adapter, run, proof)
        contracts[name] = contract
        rows.append(dict(name=name, contract=adapter.pin(row / "contract.json"),
                         oracle=adapter.pin(row / "oracle/RESULT.json"),
                         candidate=adapter.pin(row / "candidate/RESULT.json")))
    supplement = directory / "flags"
    supplement.mkdir()
    oracle_checks = flags_phase(contracts["copy-command"], "oracle", supplement / "oracle",
                                plan, adapter, proof)
    frozen = dict(receipt=adapter.pin(supplement / "oracle/RESULT.json"), plan=adapter.pin(FLAGS))
    adapter.save(supplement / "ORACLE-FROZEN.json", frozen)
    adapter.authenticate(frozen["receipt"])
    adapter.authenticate(frozen["plan"])
    candidate_checks = flags_phase(contracts["copy-command"], "candidate", supplement / "candidate",
                                   plan, adapter, proof)
    adapter.require(oracle_checks == candidate_checks, "native oracle/candidate H/U mismatch")
    adapter.require(identity.verify_built(candidate, source, head, target) == proof,
                    "option candidate identity changed")
    adapter.save(directory / "RESULT.json", dict(head=head, os="linux", options=153,
                 phases=306, flags_per_phase=58, status="PASS", candidate_identity=proof,
                 rows=rows, flags={phase: adapter.pin(supplement / phase / "RESULT.json")
                                   for phase in ("oracle", "candidate")}))
    print(f"[regression] options: 153/153, 306 phases, H/U 58/58; {directory}", flush=True)
