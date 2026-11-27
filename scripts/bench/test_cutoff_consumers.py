#!/usr/bin/env python3
"""Pure adversarial tests: synthetic receipts never count as native evidence."""
import copy
import io
import json
import hashlib
import os
import subprocess
import sys
from pathlib import Path
import tarfile
import tempfile
import unittest
import zipfile
import cutoff_assets as assets
import windows_cutoff_consumer as windows


def synthetic():
    pin = {"path": "synthetic-only", "bytes": 1, "sha256": "a" * 64}
    cases = [{"case_id": branch + "-" + kind, "kind": kind, "branch_scope": branch,
              "geometry_before": [80, 24], "geometry_after": [90, 25]}
             for branch in ("immediate", "deferred") for kind in ("inbox", "companion")]
    expected = [{"run_id": "synthetic", "operation": c["case_id"], "tool": "rmux",
                 "pair": 1, "order": 0, "sequence": i+1,
                 "attempt_id": "synthetic/" + c["case_id"], "attempt_number": 1}
                for i, c in enumerate(cases)]
    contract = {"schema": 1, "source": "b" * 40, "observer_qualification": pin,
                "binary": pin, "build_provenance": pin, "cases": cases, "expected": expected}
    data = {"schema": 1, "scope": "product-conpty-calls", "source": contract["source"],
            "contract_sha256": windows.qualification.digest(contract), "binary": pin,
            "build_provenance": pin, "observer": pin, "raw_evidence": pin,
            "observer_qualification": pin, "attempts": [dict(x, status="pass", elapsed_ms=1.0) for x in expected],
            "operations": [{"id": c["case_id"], "metrics": {"rmux": {"samples_ms": [1.0]}}} for c in cases],
            "notes": [], "complete": True, "flushed": True, "dropped": 0, "write_errors": 0,
            "cap_hit": False, "producer_birth_filetime": 100, "qpc_frequency": 1000,
            "window_start": 1, "window_end": 1000, "attempted_calls": 8, "written_calls": 8, "cases": []}
    for planned in cases:
        identity = {k: 1 for k in ("session_id", "window_id", "pane_id", "child_pid", "birth_filetime")}
        case = dict(planned, identity_before=identity, identity_after=identity,
                    child_same_identity_alive_after_cleanup=False, pty_id=1, calls=[])
        for i, api in enumerate(("create", "resize")):
            dimensions = planned["geometry_before" if api == "create" else "geometry_after"]
            case["calls"].append({"sequence": i+1, "operation_id": i+1, "scope": "product-conpty-call",
                                  "case_id": planned["case_id"], "pty_id": 1,
                                  "begin_dimensions": dimensions, "end_dimensions": dimensions,
                                  "api": api, "begin_qpc": 10+i*10, "end_qpc": 11+i*10,
                                  "qpc_frequency": 1000, "producer_birth_filetime": 100,
                                  "module": dict(pin, kind=planned["kind"], provenance_available=True, file_version="1.24.2607.10001"),
                                  "hresult": 0, "flags": 0})
        data["cases"].append(case)
    return contract, data


class Migration(unittest.TestCase):
    def test_consistent_synthetic_is_not_native_proof(self):
        result = windows.consume(*synthetic())
        self.assertEqual(result["status"], "PASS_RECORD_CONSISTENCY_ONLY")
        self.assertFalse(result["native_proof_inferred"])

    def test_zero_based_product_ids_and_recorded_fallback_are_valid(self):
        c,d = synthetic()
        for case in d["cases"]:
            for identity in (case["identity_before"], case["identity_after"]):
                identity.update(session_id=0, window_id=0, pane_id=0)
            case["pty_id"] = 0
            for call in case["calls"]:
                call["pty_id"] = 0
        # A companion failure followed by inbox success is retained as an
        # observed retry, not transformed into a successful first call.
        case = d["cases"][1]
        first = copy.deepcopy(case["calls"][0]);first["hresult"] = -1
        first["sequence"] = 1;first["operation_id"] = 100
        for call in case["calls"]:
            call["sequence"] += 1
            call["module"]["kind"] = "inbox"
        case["calls"].insert(0, first)
        d["attempted_calls"] = d["written_calls"] = 9
        self.assertEqual(windows.consume(c,d)["status"], "PASS_RECORD_CONSISTENCY_ONLY")

    def test_original_tick_tolerance_and_intermediate_geometry_are_retained(self):
        c,d = synthetic()
        call=d["cases"][0]["calls"][1]
        call["end_qpc"]=call["begin_qpc"]-1
        call["begin_dimensions"] = call["end_dimensions"] = [100,30]
        # These are actual intermediate API arguments, not the later public
        # geometry. The old consumer checked paired equality, not equality
        # of every intermediate to the final public observation.
        self.assertEqual(windows.consume(c,d)["status"], "PASS_RECORD_CONSISTENCY_ONLY")
        call["end_qpc"]-=1
        self.assertEqual(windows.consume(c,d)["status"], "FAIL")
        c,d = synthetic();del d["cases"][0]["calls"][0]["flags"]
        self.assertEqual(windows.consume(c,d)["status"], "FAIL")

    def test_declared_branch_must_be_present_and_match(self):
        for replacement in (None, "immediate"):
            c,d = synthetic()
            case=d["cases"][2]
            if replacement is None:
                del case["branch_scope"]
            else:
                case["branch_scope"] = replacement
            result=windows.consume(c,d)
            self.assertEqual(result["status"], "FAIL")
            self.assertIn("missing or wrong observed branch", result["errors"])

    def test_same_pty_creation_and_clock_order_required(self):
        c,d = synthetic()
        calls=d["cases"][0]["calls"]
        calls.reverse()
        for i,call in enumerate(calls):
            call["sequence"]=i+1
        result=windows.consume(c,d)
        self.assertEqual(result["status"], "FAIL")
        self.assertIn("resize precedes successful same-PTY creation", result["errors"])
        c,d = synthetic()
        resize=d["cases"][0]["calls"][1]
        resize["begin_qpc"],resize["end_qpc"]=2,3
        result=windows.consume(c,d)
        self.assertEqual(result["status"], "FAIL")
        self.assertIn("same-PTY attempt chronology differs", result["errors"])
        c,d = synthetic();d["cases"][0]["calls"][0]["hresult"]=-1
        result=windows.consume(c,d)
        self.assertIn("resize precedes successful same-PTY creation", result["errors"])

    def test_missing_or_substituted_observations_fail(self):
        def changed(path, value):
            c, d = synthetic()
            at = d
            for key in path[:-1]:
                at = at[key]
            at[path[-1]] = value
            return c, d
        mutations = [
            (["scope"], "whole-cli-qpc"), (["scope"], "standalone-conpty-probe"),
            (["source"], "c"*40), (["contract_sha256"], "d"*64),
            (["observer_qualification"], None), (["cases"], []), (["attempts"], []),
            (["notes"], ["failed sample"]), (["dropped"], 1), (["write_errors"], 1),
            (["cap_hit"], True), (["complete"], False), (["flushed"], False),
            (["qpc_frequency"], 0), (["producer_birth_filetime"], 0),
            (["written_calls"], 7), (["cases",0,"calls"], []),
            (["cases",0,"child_same_identity_alive_after_cleanup"], None),
            (["cases",0,"geometry_after"], [80,24]),
            (["cases",0,"calls",0,"hresult"], None),
            (["cases",0,"calls",0,"hresult"], 1),
            (["cases",0,"calls",0,"hresult"], False),
            (["cases",0,"calls",0,"begin_qpc"], 999),
            (["cases",0,"calls",0,"pty_id"], 2),
            (["cases",0,"calls",0,"sequence"], 2),
            (["cases",0,"calls",0,"end_dimensions"], [1,1]),
            (["cases",0,"calls",0,"module","file_version"], ""),
            (["cases",0,"calls",0,"module","sha256"], "invalid"),
            (["cases",0,"calls",0,"module","provenance_available"], False),
            (["cases",0,"calls",0,"scope"], "CLI")]
        for path, value in mutations:
            with self.subTest(path=path, value=value):
                self.assertEqual(windows.consume(*changed(path,value))["status"], "FAIL")
        c,d = synthetic();d["attempts"].append(copy.deepcopy(d["attempts"][0]))
        self.assertEqual(windows.consume(c,d)["status"], "FAIL")
        c,d = synthetic();d["cases"][0]["calls"][1]["operation_id"] = 1
        self.assertEqual(windows.consume(c,d)["status"], "FAIL")

    def test_shared_accounting_detects_sample_rewrite(self):
        c,d = synthetic();d["operations"][0]["metrics"]["rmux"]["samples_ms"] = [0.0]
        self.assertEqual(windows.consume(c,d)["status"], "FAIL")


    def test_c06_creator_lifetime_consume_and_real_cli(self):
        # One matrix exercises both public entry points with actual disk pins.
        # Optional evidence retention is for bounded qualification receipts.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(os.environ.get("CUTOFF_TEST_EVIDENCE", temporary))
            root.mkdir(parents=True, exist_ok=True)
            blob = root / "synthetic-observer-bytes"
            blob.write_bytes(b"synthetic only; never a qualified native observer")
            pinned = {"path": str(blob), "bytes": blob.stat().st_size,
                      "sha256": hashlib.sha256(blob.read_bytes()).hexdigest()}
            forwarded = root / "forwarded-module"
            forwarded.write_bytes(b"different synthetic module bytes")
            forward_pin = {"path": str(forwarded), "bytes": forwarded.stat().st_size,
                           "sha256": hashlib.sha256(forwarded.read_bytes()).hexdigest()}

            def fixture():
                c, d = synthetic()
                c["schema"] = d["schema"] = 2
                for key in ("binary", "build_provenance", "observer_qualification"):
                    c[key] = d[key] = pinned.copy()
                d["observer"] = d["raw_evidence"] = pinned.copy()
                for n, (planned, case) in enumerate(zip(c["cases"], d["cases"])):
                    case["pty_id"] = n
                    for identity in (case["identity_before"], case["identity_after"]):
                        identity.update(session_id=0, window_id=0, pane_id=0)
                    if planned["branch_scope"] == "deferred":
                        planned["geometry_after"] = case["geometry_after"] = [100, 30]
                    for i, call in enumerate(case["calls"]):
                        call.update(pty_id=n, operation_id=i, lifetime_id=0,
                                    creator_operation_id=0, hpcon=101+n, api_family=case["kind"])
                        call["module"].update(pinned)
                        if i == 1:
                            call["begin_dimensions"] = call["end_dimensions"] = [80, 24]
                return c, d

            def recreation(c, d, reused_handle=False):
                calls = d["cases"][0]["calls"]
                create, resize = copy.deepcopy(calls)
                handle = 101 if reused_handle else 202
                create.update(sequence=3, operation_id=2, creator_operation_id=2,
                              lifetime_id=1, hpcon=handle, begin_qpc=30, end_qpc=31)
                resize.update(sequence=4, operation_id=3, creator_operation_id=2,
                              lifetime_id=1, hpcon=handle, begin_qpc=40, end_qpc=41)
                calls.extend([create, resize])

            scenarios = []
            def add(name, edit=None, error=None):
                c, d = fixture()
                if edit:
                    edit(c, d)
                d["attempted_calls"] = d["written_calls"] = sum(len(x["calls"]) for x in d["cases"])
                d["contract_sha256"] = windows.qualification.digest(c)
                scenarios.append((name, c, d, error))

            add("zero-ids-intermediate-deferred-concurrent")
            def fallback(c, d):
                case = d["cases"][1]
                failed = copy.deepcopy(case["calls"][0])
                failed.update(operation_id=10, hresult=-1, lifetime_id=None,
                              creator_operation_id=None, hpcon=None)
                for call in case["calls"]:
                    call["sequence"] += 1
                    call["api_family"] = call["module"]["kind"] = "inbox"
                case["calls"].insert(0, failed)
            add("failed-companion-inbox-fallback", fallback)
            add("inbox-recreation", recreation)
            add("reused-numeric-handle-new-lifetime", lambda c,d: recreation(c,d,True))
            add("forwarded-same-family", lambda c,d: d["cases"][0]["calls"][1]["module"].update(forward_pin))
            add("one-tick-local", lambda c,d: d["cases"][0]["calls"][1].update(begin_qpc=10,end_qpc=9))
            def failed_recreate(c,d):
                recreation(c,d)
                calls=d["cases"][0]["calls"]
                calls[2].update(hresult=-1,lifetime_id=None,creator_operation_id=None,hpcon=None)
                calls[3].update(lifetime_id=0,creator_operation_id=0,hpcon=101)
            add("failed-recreation-preserves-state", failed_recreate)
            def wrong_family(c,d):
                call=d["cases"][0]["calls"][1]
                call["api_family"]=call["module"]["kind"]="companion"
            add("wrong-family-no-recreation", wrong_family, "resize API family differs")
            add("failed-resize-wrong-family", lambda c,d: (wrong_family(c,d),d["cases"][0]["calls"][1].update(hresult=-1)), "resize API family differs")
            for field, value in (("lifetime_id",1),("creator_operation_id",7),("hpcon",202)):
                add("wrong-"+field, lambda c,d,k=field,v=value: d["cases"][0]["calls"][1].update({k:v}), "active creator/lifetime/handle")
            add("wrong-pty", lambda c,d: d["cases"][0]["calls"][1].update(pty_id=1), "wrong PTY")
            add("split-same-pty-case", lambda c,d: d["cases"][1].update(pty_id=0), "duplicate or invalid PTY case")
            add("stale-lifetime-after-recreation", lambda c,d: (recreation(c,d,True),d["cases"][0]["calls"][-1].update(lifetime_id=0,creator_operation_id=0)), "active creator/lifetime/handle")
            add("reused-lifetime-id", lambda c,d: (recreation(c,d),d["cases"][0]["calls"][2].update(lifetime_id=0)), "reused lifetime")
            add("wrong-create-creator", lambda c,d: d["cases"][0]["calls"][0].update(creator_operation_id=7), "wrong creator operation")
            add("missing-lifetime", lambda c,d: d["cases"][0]["calls"][0].pop("lifetime_id"), "lifetime_id")
            add("boolean-handle", lambda c,d: d["cases"][0]["calls"][0].update(hpcon=True), "invalid native lifetime")
            add("wrong-family-attribution", lambda c,d: d["cases"][0]["calls"][0].update(api_family="companion"), "API family differs from module")
            add("two-tick-local", lambda c,d: d["cases"][0]["calls"][1].update(begin_qpc=9,end_qpc=10), "attempt chronology")
            add("missing-branch", lambda c,d: d["cases"][2].pop("branch_scope"), "observed branch")
            def legacy(c,d):
                c["schema"]=d["schema"]=1
                for case in d["cases"]:
                    for call in case["calls"]:
                        for field in windows.LIFETIME_FIELDS:
                            del call[field]
            add("schema1-single-creation", legacy)
            add("schema1-wrong-family", lambda c,d: (wrong_family(c,d),legacy(c,d)), "resize API family differs")
            add("schema1-recreation-requires-version", lambda c,d: (recreation(c,d),legacy(c,d)), "recreation requires schema2")
            add("schema1-unversioned-fields", lambda c,d: (c.update(schema=1),d.update(schema=1)), "lifetime fields require schema2")
            reports=[]
            for name,c,d,error in scenarios:
                with self.subTest(name=name):
                    cp,dp,rp = [root/(name+suffix+".json") for suffix in ("-contract","-records","-result")]
                    cp.write_text(json.dumps(c,indent=2)+"\n")
                    dp.write_text(json.dumps(d,indent=2)+"\n")
                    consumed=windows.consume(c,d)
                    command=[sys.executable,str(Path(windows.__file__)),"--contract",str(cp),"--evidence",str(dp),"--output",str(rp)]
                    cli=subprocess.run(command,capture_output=True,text=True,timeout=10)
                    result=json.loads(rp.read_text())
                    expected="FAIL" if error else "PASS_RECORD_CONSISTENCY_ONLY"
                    reports.append(dict(name=name,expected=expected,consume=consumed,argv=command,rc=cli.returncode,cli=result))
                    self.assertEqual(consumed["status"],expected)
                    self.assertEqual(result["status"],expected)
                    self.assertEqual(cli.returncode,int(error is not None),cli.stderr)
                    self.assertFalse(result["native_proof_inferred"])
                    if error:
                        self.assertIn(error,"; ".join(consumed["errors"]))
            (root/"C06-MATRIX.json").write_text(json.dumps(reports,indent=2)+"\n")


class Assets(unittest.TestCase):
    def archive(self, path, name, data):
        with tarfile.open(path, "w:gz") as out:
            info=tarfile.TarInfo(name);info.size=len(data);out.addfile(info,io.BytesIO(data))

    def test_nested_archive_content_cannot_hide_behind_a_payload_name(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for outer_kind in ("tar", "zip"):
                for inner_kind in ("tar", "zip"):
                    with self.subTest(outer=outer_kind, inner=inner_kind):
                        inner = root / "inner"
                        if inner_kind == "tar":
                            self.archive(inner, "src/recorder.rs", b"RMUX_PERF_TRACE")
                        else:
                            with zipfile.ZipFile(inner, "w", zipfile.ZIP_DEFLATED) as out:
                                out.writestr("src/recorder.rs", b"RMUX_PERF_TRACE")
                        outer = root / "package"
                        if outer_kind == "tar":
                            self.archive(outer, "resources/payload.bin", inner.read_bytes())
                        else:
                            with zipfile.ZipFile(outer, "w", zipfile.ZIP_DEFLATED) as out:
                                out.writestr("resources/payload.bin", inner.read_bytes())
                        report = assets.scan(outer)
                        self.assertEqual(report["status"], "FAIL")
                        leaf = next(row for row in report["members"]
                                    if row["name"] == "resources/payload.bin!src/recorder.rs")
                        self.assertEqual(leaf["findings"][0]["marker"], "RMUX_PERF_TRACE")

    def test_bytes_encodings_paths_and_nested_crates(self):
        with tempfile.TemporaryDirectory() as directory:
            r=Path(directory);p=r/"payload.tar.gz"
            self.archive(p,"bin/rmux",b"clean")
            self.assertEqual(assets.scan(p)["status"],"PASS")
            for marker in assets.MARKERS:
                for encoding in ("utf-8","utf-16-le","utf-16-be"):
                    with self.subTest(marker=marker,encoding=encoding):
                        self.archive(p,"bin/rmux",marker.encode(encoding))
                        self.assertEqual(assets.scan(p)["status"],"FAIL")
                self.archive(p,"src/"+marker+".rs",b"clean")
                self.assertEqual(assets.scan(p)["status"],"FAIL")
            inner=r/"rmux-pty.crate";self.archive(inner,"src/initial_trace.rs",b"restored")
            self.archive(p,"crates/rmux-pty.crate",inner.read_bytes())
            self.assertEqual(assets.scan(p)["status"],"FAIL")
            z=r/"payload.zip"
            with zipfile.ZipFile(z,"w",zipfile.ZIP_DEFLATED) as out:
                out.writestr("rmux.exe",b"RMUX_PERF_TRACE")
            self.assertEqual(assets.scan(z)["status"],"FAIL")

    def test_missing_empty_duplicate_unsafe_archives_fail(self):
        with tempfile.TemporaryDirectory() as directory:
            r=Path(directory);p=r/"payload.tar.gz"
            with self.assertRaises(ValueError):assets.scan(r/"missing")
            for name in ("opaque.deb", "opaque.rpm", "opaque.snap", "corrupt.crate"):
                unknown=r/name;unknown.write_bytes(b"opaque compressed payload")
                with self.assertRaises(ValueError):assets.scan(unknown)
            elf=r/"rmux";elf.write_bytes(b"\x7fELF clean")
            self.assertEqual(assets.scan(elf)["status"],"PASS")
            elf.write_bytes(b"\x7fELF RMUX_PERF_TRACE")
            self.assertEqual(assets.scan(elf)["status"],"FAIL")
            self.archive(p,"../escape",b"x")
            with self.assertRaises(ValueError):assets.scan(p)
            with tarfile.open(p,"w:gz") as out:
                for _ in range(2):
                    info=tarfile.TarInfo("bin/rmux");info.size=1;out.addfile(info,io.BytesIO(b"x"))
            with self.assertRaises(ValueError):assets.scan(p)
            with tarfile.open(p,"w:gz"):pass
            with self.assertRaises(ValueError):assets.scan(p)

if __name__ == "__main__":
    unittest.main()
