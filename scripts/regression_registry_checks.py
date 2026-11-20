#!/usr/bin/env python3
"""Replay closed-line recipes through their existing qualified tools."""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

import candidate_identity as identity


ROOT = Path(__file__).resolve().parents[1]


def run(argv, **kwargs):
    argv = [str(arg) for arg in argv]
    if not Path(argv[0]).is_absolute():
        program = shutil.which(argv[0])
        if not program:
            raise RuntimeError("missing protective tool: " + argv[0])
        argv[0] = str(Path(program).absolute())
    return subprocess.run(argv, cwd=ROOT, check=True, **kwargs)


def web_no_listener(target):
    # Same CLI/SDK operations and twenty-second bound as the retained probe.
    cargo = shutil.which("cargo")
    if not cargo:
        raise RuntimeError("missing protective tool: cargo")
    built = run([cargo, "build", "--locked", "--offline", "-p", "rmux", "--bin", "rmux",
                 "--features", "rmux-sdk/default", "--message-format=json"],
                stdout=subprocess.PIPE, text=True)
    libraries = {}
    binaries = []
    for line in built.stdout.splitlines():
        item = json.loads(line)
        if item.get("reason") != "compiler-artifact":
            continue
        name = item["target"]["name"]
        if name in ("rmux_sdk", "tokio"):
            libraries.setdefault(name, set()).update(
                Path(path) for path in item["filenames"] if path.endswith(".rlib"))
        if name == "rmux" and item.get("executable"):
            binaries.append(Path(item["executable"]))
    if len(binaries) != 1 or any(len(libraries.get(name, [])) != 1 for name in ("rmux_sdk", "tokio")):
        raise RuntimeError("CLI/SDK build did not identify unique link inputs")
    libraries = {name: next(iter(paths)) for name, paths in libraries.items()}
    helper = target / "debug/web_no_listener_sdk"
    run([sys.executable, ROOT / "scripts/candidate_identity.py", "rustc",
         os.environ["RMUX_IDENTITY_RUSTC"], "--crate-name", "web_no_listener_sdk",
         "--crate-type", "bin", "--edition=2021", ROOT / "scripts/fixtures/web_no_listener_sdk.rs",
         "--emit=link,dep-info", "--extern", f"rmux_sdk={libraries['rmux_sdk']}",
         "--extern", f"tokio={libraries['tokio']}", "-L",
         f"dependency={libraries['tokio'].parent}", "-o", helper])
    # All actual images, including CLI daemon spawns, use the retained guard.
    guard = [sys.executable, ROOT / "scripts/candidate_identity.py", "run"]
    # Transient sockets need a short pathname within sockaddr_un's limit.
    with tempfile.TemporaryDirectory(prefix="rmux-no-port-") as directory:
        socket = Path(directory) / "s"
        base = [*guard, binaries[0], "-S", socket, "-f", "/dev/null"]
        env = dict(os.environ, RMUX_DISABLE_TMUX_FALLBACK="1")
        for key in ("RMUX", "TMUX", "RMUX_INTERNAL_BINARY_PATH"):
            env.pop(key, None)

        def cli(*args):
            return run([*base, *args], text=True, stdout=subprocess.PIPE,
                       stderr=subprocess.PIPE, timeout=20, env=env)

        def listeners(pid):
            sockets = set()
            for fd in Path(f"/proc/{pid}/fd").iterdir():
                try:
                    link = os.readlink(fd)
                except FileNotFoundError:
                    continue
                if link.startswith("socket:["):
                    sockets.add(link[8:-1])
            return sum(row.split()[3] == "0A" and row.split()[9] in sockets
                       for name in ("tcp", "tcp6")
                       for row in Path(f"/proc/{pid}/net/{name}").read_text().splitlines()[1:])

        pid = None
        try:
            cli("new-session", "-d", "-s", "web-neighbors", "cat")
            pid = int(cli("display-message", "-p", "#{pid}").stdout.strip())
            if listeners(pid):
                raise RuntimeError("daemon already has a TCP listener")
            for args in (("list",), ("lookup", "missing"), ("stop", "missing"), ("stop", "all")):
                cli("web-share", *args)
                if listeners(pid):
                    raise RuntimeError(f"CLI {args[0]} opened a TCP listener")
            for operation in ("list", "lookup", "stop", "stop-all"):
                run([*guard, helper, socket, operation], timeout=20, env=env)
                if listeners(pid):
                    raise RuntimeError(f"SDK {operation} opened a TCP listener")
            print("[regression] CLI/SDK no-port operations: 8/8")
        finally:
            cli("kill-server")
        deadline = time.monotonic() + 20
        while pid is not None and Path(f"/proc/{pid}").exists():
            if time.monotonic() >= deadline:
                raise RuntimeError("daemon remains after kill-server")
            time.sleep(0.05)


def formats_corpus(target):
    if sys.platform != "linux":
        raise RuntimeError("independent corpus requires its accepted Linux profile")
    run(["cargo", "build", "--locked", "--offline", "--no-default-features",
         "-p", "rmux", "--bin", "rmux", "--bin", "rmux-daemon"])
    selected = run([sys.executable, ROOT / "scripts/candidate_identity.py", "check",
                    target / "debug/rmux"], stdout=subprocess.PIPE, text=True)
    source, head, _ = identity.context()
    proof = identity.verify_built(Path(selected.stdout.strip()), source, head, target)
    built = json.loads(Path(proof["build"]).read_text())
    candidate = Path(built["producer"] if built["kind"] == "unix-launcher" else built["path"])
    identity.verify_built(candidate, source, head, target)
    audit = ROOT / ".rmux-audit"
    audit.mkdir(exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="formats-", dir=audit))
    contract, plan = directory / "contract.json", directory / "plan.json"
    oracle, result = directory / "o", directory / "c"
    command = [sys.executable, "-B", ROOT / "tests/format_jobs_reference.py"]
    run([*command, "bounded-corpus",
         ROOT / "tests/format_jobs_reference/formats_independent_routes.json",
         source, head, "/usr/local/bin/tmux", candidate, contract])
    run([*command, "bounded-reader", "plan", contract,
         hashlib.sha256(contract.read_bytes()).hexdigest(), oracle, result, plan])
    flags = ["--reader-plan", plan, "--reader-sha256",
             hashlib.sha256(plan.read_bytes()).hexdigest()]
    run([*command, "bounded-linux", contract, "oracle", oracle, *flags])
    # INT118's accepted bridge freezes the new oracle receipt before replay.
    def pin(path):
        data = path.read_bytes()
        return dict(path=str(path), bytes=len(data), sha256=hashlib.sha256(data).hexdigest())
    (oracle / "frozen.json").write_text(json.dumps(dict(
        receipt=pin(oracle / "RESULT.json"), contract=pin(contract))) + "\n")
    run([*command, "bounded-linux", contract, "candidate", result,
         "--oracle", oracle / "frozen.json", *flags])
    run([sys.executable, "-B", "-m", "unittest", "discover", "-s",
         "tests/format_jobs_reference", "-v"])


LEDGER_EVIDENCE = "RMUX_LEDGER_EVIDENCE"
# D65/D124 proof canaries: with a declared ledger proof they must run, never skip.
LEDGER_PROOF_TESTS = ("test_d124_exact_reviewed_attachments_and_explicit_open_remainder",
                      "test_int148_real_composition_and_settlements",
                      "test_false_witness_and_unresolvable_proof_are_rejected")
UNITTEST_RAN = re.compile(r"^Ran (\d+) tests? in \d+(?:\.\d+)?s$", re.MULTILINE)
UNITTEST_OK = re.compile(r"\A\s*OK(?: \(([^()\n]*)\))?\s*\Z")


def executed_tests(output):
    """Tests unittest actually executed: its final Ran total minus its skips."""
    summaries = list(UNITTEST_RAN.finditer(output))
    if not summaries:
        return None
    verdict = UNITTEST_OK.match(output[summaries[-1].end():])
    if not verdict:
        return None
    details = dict(item.split("=", 1) for item in (verdict[1] or "").split(", ") if "=" in item)
    return int(summaries[-1][1]) - int(details.get("skipped", 0))


def inventory_population_complete(output, proofs_declared):
    """At least 50 executed tests, and every canary passed rather than skipped."""
    required = ("test_new_maintained_tooling_and_exact_size_boundary",
                "test_cli_size_ratchet_and_reduction_checks_fail",
                "test_d65_blacklist_and_production_ratchet",
                *(LEDGER_PROOF_TESTS if proofs_declared else ()))
    executed = executed_tests(output)
    return executed is not None and executed >= 50 and all(
        re.search(r"^" + name + r" \([^\n]+\) \.\.\. ok$", output, re.MULTILINE)
        for name in required)


def declared_proofs(value):
    """Every content-addressed proof locator a ledger declares."""
    if isinstance(value, dict):
        for child in value.values():
            yield from declared_proofs(child)
    elif isinstance(value, list):
        for child in value:
            yield from declared_proofs(child)
    elif isinstance(value, str) and value.startswith("sha256:"):
        yield value


def require_ledger_evidence(ledger, environ):
    """A ledger declaring proofs requires RMUX_LEDGER_EVIDENCE holding all of them."""
    locators = set(declared_proofs(ledger))
    if not locators:
        return None
    value = environ.get(LEDGER_EVIDENCE)
    if value is None:
        raise RuntimeError(f"declared ledger proofs require {LEDGER_EVIDENCE}")
    root = Path(value)
    if not root.is_absolute() or not root.is_dir():
        raise RuntimeError(f"{LEDGER_EVIDENCE} must name an absolute proof directory")
    inventory = str(ROOT / "scripts/source-inventory")
    if inventory not in sys.path:
        sys.path.insert(0, inventory)
    import ledger_evidence
    for locator in sorted(locators):
        try:
            ledger_evidence.resolve(ROOT, ledger, locator, root)
        except ValueError as error:
            raise RuntimeError(f"ledger proof {locator}: {error}") from error
    return root


def source_inventory(target):
    if sys.platform not in ("darwin", "linux", "win32"):
        raise RuntimeError("source inventory requires an accepted native profile")
    ledger = json.loads((ROOT / "scripts/source-inventory/production-ledger.json").read_text())
    evidence = require_ledger_evidence(ledger, os.environ)
    source, head, _ = identity.context()
    manifest = source / "scripts/source-inventory/Cargo.toml"
    unit = identity.support("candidate_identity_manifest").binding(identity, source, head, target)
    if unit is None:
        # D57 owns a distinct compilation context; keep the outer gate intact.
        env = {key: value for key, value in os.environ.items()
               if not key.startswith(("RMUX_IDENTITY_", "CARGO_TARGET_"))
               and key not in ("CARGO_TARGET_DIR", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER")}
        env.update(RUSTC=os.environ["RMUX_IDENTITY_RUSTC"],
                   RUSTDOC=os.environ["RMUX_IDENTITY_RUSTDOC"])
        run([sys.executable, ROOT / "scripts/candidate_identity.py", "with-manifest",
             manifest, "--", sys.executable, Path(__file__).resolve(), "source-inventory"], env=env)
        return
    if unit["manifest"] != str(manifest):
        raise RuntimeError("source inventory requires its own immutable parser workspace")
    run(["cargo", "build", "--locked", "--offline", "--manifest-path", manifest, "--jobs", "2"])
    parser = target / ("debug/rmux-source-syntax.exe" if sys.platform == "win32"
                       else "debug/rmux-source-syntax")
    proof = identity.verify_built(parser, source, head, target)
    selected = run([sys.executable, ROOT / "scripts/candidate_identity.py", "check", parser],
                   stdout=subprocess.PIPE, text=True)
    env = dict(os.environ, SOURCE_SYNTAX=selected.stdout.strip(),
               SOURCE_AUDIT=str(target / "source-fixtures"))
    if sys.platform == "win32":
        env["PYTHONUTF8"] = "1"
    argv = [sys.executable, "-B", "-m", "unittest", "discover", "-s",
            "scripts/source-inventory", "-p", "test*.py", "-v"]
    tested = subprocess.run(argv, cwd=ROOT, env=env, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    print(tested.stdout, end="", flush=True)
    if tested.returncode:
        raise subprocess.CalledProcessError(tested.returncode, argv)
    if not inventory_population_complete(tested.stdout, evidence is not None):
        raise RuntimeError("accepted source inventory population or canaries missing")
    if identity.verify_built(parser, source, head, target) != proof:
        raise RuntimeError("source parser identity changed during regression")


def options_bank(target):
    if sys.platform != "linux":
        raise RuntimeError("options bank requires its accepted Linux profile")
    import regression_registry as registry
    command = next(item for item in registry.load_registry()
                   if item["id"] == "formats-core-options-ignored")
    argv = command["test_argv"]
    if argv[argv.index("--") + 1:] != ["--ignored", "--exact"]:
        raise RuntimeError("options bank requires the accepted ignored/exact selection")
    directory = Path(tempfile.mkdtemp(prefix="options-bank-", dir=target))
    output = directory / "options-bank.json"
    oracle = Path(os.environ.get("RMUX_FROZEN_OPTIONS_TABLE",
                                 "/opt/rmux/reference/tmux/options-table.c")).resolve(strict=True)
    env = dict(os.environ, RMUX_FROZEN_OPTIONS_TABLE=str(oracle),
               RMUX_OPTIONS_BANK_OUTPUT=str(output))
    # The retained Rust test authenticates the oracle and creates a fresh export.
    code = registry.run_commands([dict(command, argv=argv)], ROOT, env=env)
    if code:
        raise subprocess.CalledProcessError(code, argv)
    document = json.loads(output.read_text())
    if (document["target_os"] != "linux" or document["registry_count"] < 1
            or document["oracle"]["source_sha256"] != hashlib.sha256(oracle.read_bytes()).hexdigest()):
        raise RuntimeError("options bank export missing or differs from its native oracle")
    print(f"[regression] options bank fresh export: {output}", flush=True)


def main():
    identity.validate_context()
    _, _, target = identity.context()
    mode = sys.argv[1]
    if mode == "web-no-listener":
        if sys.platform != "linux":
            raise RuntimeError("no-port probe requires Linux /proc")
        web_no_listener(target)
    elif mode == "notice-build":
        cargo = shutil.which("cargo")
        if not cargo:
            raise RuntimeError("missing protective tool: cargo")
        run([sys.executable, ROOT / "scripts/packaging/notices.py", "build",
             "--binary-dir", target / "debug", "--", cargo,
             "build", "--locked", "--offline", "-p", "rmux"])
        run([sys.executable, ROOT / "scripts/packaging/notices.py", "verify",
             "--binary-dir", target / "debug", "--required"])
    elif mode == "threat-model":
        registry = json.loads((ROOT / "scripts/regression_registry.json").read_text())
        expected = registry["threat_model"]
        data = (ROOT / expected["path"]).read_bytes().split(expected["heading"].encode(), 1)[1]
        if hashlib.sha256(data).hexdigest() != expected["sha256"]:
            raise RuntimeError("reviewed threat model changed; update its independent receipt")
    elif mode == "formats-corpus":
        formats_corpus(target)
    elif mode == "source-inventory":
        source_inventory(target)
    elif mode == "options-bank":
        options_bank(target)
    elif mode == "options-per-option":
        from regression_registry_options import replay
        replay(target)
    else:
        raise RuntimeError("unknown protective check")


class PopulationAndEvidenceRules(unittest.TestCase):
    """Run with: python -B -m unittest regression_registry_checks (from scripts/)."""

    def test_skipped_tests_do_not_count_as_executed(self):
        summary = "Ran {} tests in 0.5s\n\nOK{}\n"
        self.assertEqual(executed_tests(summary.format(52, "")), 52)
        self.assertEqual(executed_tests(summary.format(52, " (skipped=3)")), 49)
        self.assertEqual(executed_tests(summary.format(53, " (skipped=3, expected failures=1)")), 50)
        self.assertEqual(executed_tests(summary.format(99, "") + summary.format(52, " (skipped=3)")), 49)
        for output in ("", "Ran 60 tests in 0.5s\n\nFAILED (failures=1)\n", "Ran 60 tests\n"):
            with self.subTest(output=output):
                self.assertIsNone(executed_tests(output))
        canaries = ("test_new_maintained_tooling_and_exact_size_boundary",
                    "test_cli_size_ratchet_and_reduction_checks_fail",
                    "test_d65_blacklist_and_production_ratchet")
        lines = "".join(f"{name} (m.C.{name}) ... ok\n" for name in canaries)
        complete = inventory_population_complete
        self.assertTrue(complete(lines + summary.format(50, ""), False))
        self.assertTrue(complete(lines + summary.format(53, " (skipped=3)"), False))
        self.assertFalse(complete(lines + summary.format(52, " (skipped=3)"), False))
        self.assertFalse(complete(lines + summary.format(49, ""), False))
        proofs = "".join(f"{name} (m.C.{name}) ... ok\n" for name in LEDGER_PROOF_TESTS)
        self.assertTrue(complete(lines + proofs + summary.format(60, ""), True))
        skipped = proofs.replace("... ok", "... skipped 'D-S1059-A'", 1)
        self.assertFalse(complete(lines + skipped + summary.format(60, " (skipped=1)"), True))

    def test_declared_ledger_proofs_require_their_evidence_root(self):
        real = json.loads((ROOT / "scripts/source-inventory/production-ledger.json").read_text())
        self.assertIsNotNone(next(declared_proofs(real), None))
        self.assertIsNone(require_ledger_evidence({"entries": [{"lot": 1}]}, {}))
        data = b"proof"
        locator = "sha256:" + hashlib.sha256(data).hexdigest()
        ledger = {"entries": [{"lot": 1, "review": locator}]}
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            (root / locator[7:]).write_bytes(data)
            self.assertEqual(require_ledger_evidence(ledger, {LEDGER_EVIDENCE: str(root)}), root)
            for environ, message in [({}, "proofs require " + LEDGER_EVIDENCE),
                                     ({LEDGER_EVIDENCE: ""}, "absolute"),
                                     ({LEDGER_EVIDENCE: os.path.relpath(root)}, "absolute"),
                                     ({LEDGER_EVIDENCE: str(root / "missing")}, "absolute")]:
                with self.subTest(environ=environ), self.assertRaisesRegex(RuntimeError, message):
                    require_ledger_evidence(ledger, environ)
            for name, content, message in [("absent", None, "unresolvable"),
                                           ("invalid", b"forged", "hash differs")]:
                (root / locator[7:]).unlink(missing_ok=True)
                if content is not None:
                    (root / locator[7:]).write_bytes(content)
                with self.subTest(proof=name), self.assertRaisesRegex(RuntimeError, message):
                    require_ledger_evidence(ledger, {LEDGER_EVIDENCE: str(root)})


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        sys.exit(error.returncode if error.returncode >= 0 else 128 - error.returncode)
    except (OSError, ValueError, KeyError, IndexError, RuntimeError, identity.IdentityError) as error:
        print(f"regression check: {error}", file=sys.stderr)
        sys.exit(2)
