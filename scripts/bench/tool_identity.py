"""Bounded disk identity for declared benchmark tools, not process-image attestation.

Receipts are trusted producer inputs. Hashes bind their recorded artifacts to
disk bytes; they do not independently prove that a producer built those bytes.
No process discovery, helper tracing or package-manager execution occurs here.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import posixpath
import re
import shutil
import stat
from typing import Any, Callable

VERSION_ARGS = {"rmux": ("-V",), "tmux": ("-V",), "zellij": ("--version",), "screen": ("-v",)}
UNKNOWN_VERSIONS = {"", "available", "unknown", "none"}

# Capability of this implementation, not a producer-supplied success flag.
# Ordinary Darwin NOTE_FORK carries no child PID, NOTE_TRACK is unsupported,
# and NOTE_EXEC is a coalescing notification rather than an image-history log.
DARWIN_HISTORY_CAPABILITY = {
    "backend": "owned-pid-text-vnode-and-kqueue",
    "complete_exec_fork_history": False,
    "registration_before_spawn": False,
    "fork_child_identity_in_event": False,
    "all_exec_images_in_event": False,
    "scope": "observed owned PID/birth/text-vnode and exit only",
}


class IdentityError(RuntimeError):
    pass


def encoded(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, indent=2) + "\n").encode()


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def darwin_process_image(pid: int, *, expected_parent: int | None = None) -> dict[str, Any]:
    """Observe a caller-owned PID; never discover or enumerate host processes.

    csops(CS_OPS_CDHASH) observes the process text vnode's code-directory hash.
    It is stronger than hashing proc_pidpath's current disk file, but is not a
    history of transient execs, forks, loaded libraries, or prior mappings.
    An exited process may already be unavailable even before waitpid collects it.
    Unknown observations are returned as unknown, never promoted to an identity.
    """
    import ctypes as c
    import sys
    import time
    if sys.platform != "darwin" or type(pid) is not int or pid <= 1:
        raise IdentityError("Darwin owned process PID required")

    class BSDInfo(c.Structure):
        _fields_ = [(name, c.c_uint32) for name in (
            "flags", "status", "xstatus", "pid", "ppid", "uid", "gid", "ruid",
            "rgid", "svuid", "svgid", "rfu")]
        _fields_ += [("comm", c.c_char * 16), ("name", c.c_char * 32)]
        _fields_ += [(name, c.c_uint32) for name in (
            "nfiles", "pgid", "pjobc", "tdev", "tpgid", "nice")]
        _fields_ += [("start_sec", c.c_uint64), ("start_usec", c.c_uint64)]

    lib = c.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    lib.proc_pidinfo.argtypes = [c.c_int, c.c_int, c.c_uint64, c.c_void_p, c.c_int]
    lib.proc_pidinfo.restype = c.c_int
    lib.proc_pidpath.argtypes = [c.c_int, c.c_void_p, c.c_uint32]
    lib.proc_pidpath.restype = c.c_int
    system = c.CDLL("/usr/lib/libSystem.B.dylib", use_errno=True)
    system.csops.argtypes = [c.c_int, c.c_uint, c.c_void_p, c.c_size_t]
    system.csops.restype = c.c_int

    def birth():
        info = BSDInfo()
        size = lib.proc_pidinfo(pid, 3, 0, c.byref(info), c.sizeof(info))
        if size != c.sizeof(info) or info.pid != pid:
            return None
        return {"pid": pid, "ppid": info.ppid,
                "birth": [info.start_sec, info.start_usec]}

    row: dict[str, Any] = {"pid": pid, "observed_ns": time.perf_counter_ns(),
                           "status": "unknown", "exec_history_complete": False}
    first = birth()
    row["before"] = first
    if first is None:
        row["cause"] = "owned PID not observable"
        return row
    if expected_parent is not None and first["ppid"] != expected_parent:
        row["cause"] = "owned child parent differs"
        return row
    path = c.create_string_buffer(4096)
    count = lib.proc_pidpath(pid, path, len(path))
    row["image_path"] = os.fsdecode(path.value) if count > 0 else None
    value = c.create_string_buffer(20)
    c.set_errno(0)
    status = system.csops(pid, 5, value, len(value))
    row["csops_returncode"] = status
    row["csops_errno"] = c.get_errno() if status else 0
    row["mapped_cdhash"] = value.raw.hex() if status == 0 else None
    last = birth()
    row["after"] = last
    if first == last and count > 0 and status == 0:
        row["status"] = "observed_text_vnode"
    else:
        row["cause"] = "image unavailable or PID/birth changed during observation"
    return row


def stamp(info: os.stat_result) -> dict[str, int]:
    return {"device": info.st_dev, "inode": info.st_ino, "mode": info.st_mode,
            "bytes": info.st_size, "mtime_ns": info.st_mtime_ns, "ctime_ns": info.st_ctime_ns}


def same_open_file(descriptor: os.stat_result, path: os.stat_result) -> bool:
    if os.name != "nt":
        return stamp(descriptor) == stamp(path)
    # Windows stat infers execute bits from the filename, unlike fstat.
    # Their legacy ctime fields can also differ (birth time vs change time).
    # Compare those fields only within each API; bind the APIs by file ID.
    return (os.path.samestat(descriptor, path)
            and stat.S_IFMT(descriptor.st_mode) == stat.S_IFMT(path.st_mode)
            and descriptor.st_size == path.st_size
            and descriptor.st_mtime_ns == path.st_mtime_ns)


def snapshot(declaration: dict[str, Any]) -> dict[str, Any]:
    """Resolve and hash a regular executable, rejecting a change during the read."""
    declared = declaration["declared"]
    if not declared or not declaration.get("selected"):
        raise IdentityError("declared executable unavailable")
    lookup = shutil.which(declared) if declaration["lookup"] == "PATH" else declared
    if lookup is None:
        raise IdentityError("declared PATH command unavailable")
    lexical = Path(os.path.abspath(lookup))
    selected = Path(os.path.abspath(declaration["selected"]))
    if lexical != selected:
        raise IdentityError("command resolution changed")
    link = lexical.lstat()
    resolved = lexical.resolve(strict=True)
    if not resolved.is_file() or not os.access(resolved, os.X_OK):
        raise IdentityError("executable must be a regular executable file")
    # Windows needs a separate path-API stability check because stat/fstat
    # metadata differ. Preserve Unix's descriptor-based observation window.
    path_before = resolved.stat() if os.name == "nt" else None
    with resolved.open("rb") as stream:
        before = os.fstat(stream.fileno())
        if not stat.S_ISREG(before.st_mode):
            raise IdentityError("executable is not a regular file")
        sha = hashlib.file_digest(stream, "sha256").hexdigest()
        after = os.fstat(stream.fileno())
    if path_before is not None:
        path_after = resolved.stat()
        changed = (stamp(before) != stamp(after) or stamp(path_before) != stamp(path_after)
                   or not same_open_file(after, path_after))
    else:
        changed = stamp(before) != stamp(after) or stamp(resolved.stat()) != stamp(after)
    if (changed
            or lexical.resolve(strict=True) != resolved or stamp(lexical.lstat()) != stamp(link)):
        raise IdentityError("executable changed during identity read")
    return {"status": "observed", "declared": declared, "selected": str(lexical),
            "resolved": str(resolved), "declared_type": "symlink" if stat.S_ISLNK(link.st_mode) else "file",
            "file_type": "regular", "link_stat": stamp(link), "stat": stamp(after),
            "bytes": after.st_size, "sha256": sha}


def pinned_json(reference: dict[str, Any]) -> tuple[dict[str, Any], dict[str, Any]]:
    path = Path(reference["path"]).absolute()
    raw = path.read_bytes()
    sha = digest(raw)
    if sha != reference["sha256"]:
        raise IdentityError("provenance receipt digest differs")
    return json.loads(raw), {"path": str(path), "sha256": sha, "bytes": len(raw)}


def provenance_errors(record: dict[str, Any]) -> list[str]:
    """Check the pinned producer receipt and exact named artifact, never its location."""
    try:
        provenance = record["provenance"]
        receipt = provenance["receipt_data"]
        if digest(encoded(receipt)) != provenance["canonical_sha256"]:
            return ["receipt content digest differs"]
        if (receipt["schema"] != 1 or receipt["status"] != "success"
                or type(receipt["returncode"]) is not int or receipt["returncode"] != 0
                or not isinstance(receipt["argv"], list) or not receipt["argv"]
                or not all(isinstance(arg, str) and arg for arg in receipt["argv"])):
            return ["successful producer execution receipt required"]
        artifact = receipt["artifacts"][provenance["artifact"]]
        identity = record["identity"]
        if (artifact["role"] != record["role"] or artifact["sha256"] != identity["sha256"] or type(artifact["bytes"]) is not int
                or artifact["bytes"] != identity["bytes"]):
            return ["receipt artifact does not match executable"]
        if receipt["kind"] == "source-build":
            source = receipt["source"]
            if (not re.fullmatch(r"[0-9a-f]{40}", source["commit"])
                    or not re.fullmatch(r"[0-9a-f]{40}", source["tree"])
                    or source["clean"] is not True or not isinstance(receipt["build_id"], str)
                    or receipt["build_id"].strip().lower() in UNKNOWN_VERSIONS):
                return ["complete clean source/build identity required"]
        elif receipt["kind"] == "installed-package":
            if not all(isinstance(receipt["package"][key], str)
                       and receipt["package"][key].strip().lower() not in UNKNOWN_VERSIONS
                       for key in ("name", "version", "installation")):
                return ["complete installed package identity required"]
        else:
            return ["unknown provenance kind"]
        return []
    except (KeyError, TypeError, ValueError):
        return ["provenance unknown or incomplete"]


def rmux_bindings(tools: dict[str, Any]) -> dict[str, str | None]:
    return {"client": "rmux", "public": "rmux_public" if "rmux_public" in tools else "rmux",
            "daemon": "rmux_daemon", "helper": "rmux_helper" if "rmux_helper" in tools else None}


def lexical_path(value: str, cwd: str) -> str:
    """Interpret the producer's POSIX spelling without consulting the review host."""
    if not isinstance(value, str) or not value or "\0" in value:
        raise ValueError("nonempty path required")
    return posixpath.normpath(value if posixpath.isabs(value) else posixpath.join(cwd, value))


def rmux_path_errors(manifest: dict[str, Any], binaries: dict[str, Any] | None = None) -> list[str]:
    """Bind metadata to selected paths, retaining the observed symlink pin separately."""
    errors = []
    try:
        if "rmux" not in manifest["requested_tools"]:
            return []  # Unselected RMUX metadata grants no RMUX identity qualification.
        context = manifest["path_context"]
        cwd = context["cwd"]
        if (context["schema"] != 1 or context["flavor"] != "posix"
                or not isinstance(cwd, str) or not posixpath.isabs(cwd)
                or lexical_path(cwd, "/") != cwd):
            return ["RMUX path context invalid"]
        bindings = rmux_bindings(manifest["tools"])
        if context["rmux_bindings"] != bindings:
            errors.append("RMUX path role bindings differ")
        if binaries is not None and (not isinstance(binaries, dict) or set(binaries) != set(bindings)):
            return errors + ["RMUX executable mapping incomplete"]
        for field, role in bindings.items():
            if role is None:
                if binaries is not None and binaries[field] is not None:
                    errors.append("RMUX helper path declared without frozen identity")
                continue
            record = manifest["tools"][role]
            declaration, identity = record["declaration"], record["identity"]
            if (declaration["lookup"] != "path"
                    or lexical_path(declaration["declared"], cwd) != identity["selected"]
                    or lexical_path(declaration["selected"], cwd) != identity["selected"]):
                errors.append(role + ": declared path differs from frozen selected identity")
            if binaries is not None and lexical_path(binaries[field], cwd) != identity["selected"]:
                errors.append("RMUX " + field + " path differs from frozen " + role + " identity")
        return errors
    except (KeyError, TypeError, ValueError):
        return errors + ["RMUX path context or executable mapping incomplete"]


def manifest_errors(manifest: dict[str, Any]) -> list[str]:
    errors = []
    try:
        if manifest["schema"] != 1 or not manifest["run_id"]:
            errors.append("invalid identity manifest")
        tools = manifest["tools"]
        requested = manifest["requested_tools"]
        if not requested or len(set(requested)) != len(requested) or not set(requested) <= set(VERSION_ARGS):
            errors.append("invalid requested identity population")
        if not set(requested) <= set(tools):
            errors.append("selected tool absent from identity manifest")
        for role, record in tools.items():
            if role not in {*VERSION_ARGS, "rmux_public", "rmux_helper", "rmux_daemon"} or record["role"] != role:
                errors.append("unknown or inconsistent tool role")
            identity = record["identity"]
            if (identity.get("status") != "observed" or identity.get("file_type") != "regular"
                    or not identity.get("resolved") or not re.fullmatch(r"[0-9a-f]{64}", identity.get("sha256", ""))
                    or type(identity.get("bytes")) is not int or identity["bytes"] < 0):
                errors.append(role + ": file identity unknown")
            elif (identity["declared"] != record["declaration"]["declared"]
                  or not Path(identity["selected"]).is_absolute()
                  or not Path(identity["resolved"]).is_absolute()
                  or identity["declared_type"] not in ("symlink", "file")
                  or identity["stat"]["bytes"] != identity["bytes"]
                  or not stat.S_ISREG(identity["stat"]["mode"])
                  or not all(key in identity[section] for section in ("stat", "link_stat")
                             for key in ("device", "inode", "mode", "bytes", "mtime_ns", "ctime_ns"))):
                errors.append(role + ": file identity incomplete")
            errors.extend(role + ": " + error for error in provenance_errors(record))
            if role in requested:
                version = record["version"]
                if (version["status"] != "observed" or not isinstance(version.get("value"), str)
                        or version["value"].strip().lower() in UNKNOWN_VERSIONS
                        or version.get("argv") != [identity.get("selected"), *VERSION_ARGS[role]]
                        or version.get("timeout_seconds") != 5):
                    errors.append(role + ": version unknown")
        if "rmux" in requested:
            if not {"rmux", "rmux_daemon"} <= set(tools):
                errors.append("explicit RMUX client and daemon identities required")
            rmux_roles = [record for role, record in tools.items() if role == "rmux" or role.startswith("rmux_")]
            receipts = set()
            for record in rmux_roles:
                provenance = record["provenance"]
                receipt = provenance["receipt_data"]
                if receipt["kind"] != "source-build":
                    errors.append("RMUX requires common source/build receipt")
                receipts.add(provenance["receipt"]["sha256"])
            if len(receipts) != 1:
                errors.append("RMUX client/helper/daemon are not linked by the same build receipt")
        errors.extend(rmux_path_errors(manifest))
        return errors
    except (KeyError, TypeError, ValueError):
        return errors + ["identity manifest incomplete"]


def write_once(path: Path, data: dict[str, Any]) -> dict[str, Any]:
    raw = encoded(data)
    # Exclusive creation refuses reuse of a run manifest. Failure never starts sampling.
    with path.open("xb") as stream:
        stream.write(raw)
        stream.flush()
        os.fsync(stream.fileno())
    if path.read_bytes() != raw:
        raise IdentityError("saved identity manifest differs")
    return {"path": str(path.absolute()), "sha256": digest(raw), "bytes": len(raw)}


class ToolIdentity:
    def __init__(self, run_id: str, out: Path, declarations: dict[str, dict[str, Any]],
                 requested: list[str], provenance_path: Path | None):
        self.run_id = run_id
        self.out = out
        self.declarations = declarations
        self.requested = requested
        self.provenance_path = provenance_path
        self.required = provenance_path is not None
        self.path_context = {"schema": 1, "flavor": "posix", "cwd": os.getcwd(),
                             "rmux_bindings": rmux_bindings(declarations) if "rmux" in requested else {}}
        self.manifest: dict[str, Any] = {}
        self.reference: dict[str, Any] | None = None
        self.input_pins: list[dict[str, Any]] = []
        self.preflight_errors: list[str] = []
        self.final_errors: list[str] = []
        self.checks: list[dict[str, Any]] = []
        self.finalized = False

    def prepare(self, version: Callable[..., Any]) -> None:
        supplied: dict[str, Any] = {}
        if self.provenance_path is not None:
            try:
                path = self.provenance_path.absolute()
                raw = path.read_bytes()
                supplied = json.loads(raw)
                if supplied["schema"] != 1 or not isinstance(supplied["tools"], dict):
                    raise IdentityError("invalid provenance specification")
                self.input_pins.append({"path": str(path), "sha256": digest(raw), "bytes": len(raw)})
            except (OSError, ValueError, KeyError, TypeError, IdentityError) as error:
                self.preflight_errors.append("provenance specification: " + str(error))
                supplied = {}
        records = {}
        for role, declaration in self.declarations.items():
            record: dict[str, Any] = {"role": role, "declaration": declaration, "version": {"status": "unknown", "reason": "no approved probe for this role"},
                                      "provenance": {"status": "unknown"}}
            records[role] = record
            try:
                record["identity"] = snapshot(declaration)
            except (OSError, ValueError, IdentityError) as error:
                record["identity"] = {"status": "unknown", "error": str(error)}
            try:
                entry = supplied["tools"][role]
                receipt, reference = pinned_json(entry["receipt"])
                self.input_pins.append(reference)
                record["provenance"] = {"status": "producer_receipt", "receipt": reference,
                    "receipt_data": receipt, "canonical_sha256": digest(encoded(receipt)), "artifact": entry["artifact"]}
            except (KeyError, TypeError, ValueError, OSError, IdentityError) as error:
                record["provenance"]["reason"] = str(error)
        # Refuse unproven executables before invoking even their version command.
        eligible = all(not provenance_errors(record) and record["identity"].get("status") == "observed"
                       for record in records.values())
        for role in self.requested:
            record = records[role]
            if record["identity"].get("status") != "observed" or (self.required and not eligible):
                continue
            path = record["identity"]["selected"]
            args = VERSION_ARGS[role]
            try:
                value = version(path, *args, accept_nonzero_exit=role == "screen")
                if isinstance(value, str) and value.strip().lower() not in UNKNOWN_VERSIONS:
                    record["version"] = {"status": "observed", "value": value, "argv": [path, *args], "timeout_seconds": 5}
                else:
                    record["version"] = {"status": "unknown", "reason": "version command unavailable or failed"}
            except Exception as error:
                record["version"] = {"status": "unknown", "reason": str(error)}
        self.manifest = {"schema": 1, "run_id": self.run_id, "mode": "required" if self.required else "historical", "requested_tools": self.requested,
                         "tools": records, "input_pins": self.input_pins, "path_context": self.path_context,
                         "scope": "declared files and trusted producer receipts only",
                         "transient_helpers": "unknown; not observed", "process_images": "unknown; disk snapshot only"}
        self.preflight_errors.extend(manifest_errors(self.manifest))
        self.reference = write_once(self.out.with_name(self.out.name + ".tools-" + self.run_id + ".json"), self.manifest)
        self.check("before_sampling")
        if self.required and self.preflight_errors:
            raise IdentityError("; ".join(self.preflight_errors))

    def check(self, phase: str) -> None:
        errors = []
        try:
            if self.reference is None or Path(self.reference["path"]).read_bytes() != encoded(self.manifest):
                errors.append("frozen identity manifest missing or changed")
            for pin in self.input_pins:
                if digest(Path(pin["path"]).read_bytes()) != pin["sha256"]:
                    errors.append("provenance input changed: " + pin["path"])
            for role, record in self.manifest.get("tools", {}).items():
                if record["identity"].get("status") != "observed":
                    continue
                try:
                    if snapshot(record["declaration"]) != record["identity"]:
                        errors.append(role + ": executable identity changed")
                except (OSError, ValueError, IdentityError) as error:
                    errors.append(role + ": " + str(error))
        except (OSError, ValueError) as error:
            errors.append(str(error))
        self.checks.append({"phase": phase, "errors": errors})
        if errors:
            self.final_errors.extend(errors)
            if self.required:
                raise IdentityError("; ".join(errors))

    def finish(self) -> None:
        try:
            self.check("after_sampling")
        finally:
            self.finalized = True

    def state(self) -> dict[str, Any]:
        status = "unqualified_historical"
        if self.required:
            status = "rejected" if self.preflight_errors or self.final_errors else (
                "verified_declared_tools" if self.finalized else "pending")
        return {"mode": "required" if self.required else "historical", "status": status,
                "manifest": self.reference, "manifest_data": self.manifest,
                "preflight_errors": self.preflight_errors, "final_errors": self.final_errors,
                "checks": self.checks, "finalized": self.finalized,
                "scope": "declared tools only; transient helpers and process images unknown"}


def saved_errors(state: dict[str, Any], run_id: str, requested: list[str],
                 attempts: list[dict[str, Any]], binaries: dict[str, Any]) -> list[str]:
    if state.get("mode") == "historical":
        if (state.get("status") != "unqualified_historical"
                or state.get("manifest_data", {}).get("mode") != "historical"):
            return ["historical mode cannot claim declared tool qualification"]
        return []  # R5-B completion grants no R6 qualification in this mode.
    try:
        errors = manifest_errors(state["manifest_data"])
        if state["mode"] != "required" or state["status"] != "verified_declared_tools" or state["finalized"] is not True:
            errors.append("declared tool qualification unfinished or rejected")
        if state["preflight_errors"] or state["final_errors"] or any(row["errors"] for row in state["checks"]):
            errors.append("declared tool identity errors retained")
        if state["manifest_data"]["run_id"] != run_id or state["manifest_data"]["mode"] != state["mode"]:
            errors.append("identity manifest run differs")
        if state["manifest_data"]["requested_tools"] != requested:
            errors.append("identity manifest tool selection differs")
        if not isinstance(binaries, dict):
            errors.append("RMUX executable mapping incomplete")
        else:
            errors.extend(rmux_path_errors(state["manifest_data"], binaries))
        raw = encoded(state["manifest_data"])
        if digest(raw) != state["manifest"]["sha256"] or len(raw) != state["manifest"]["bytes"]:
            errors.append("identity manifest digest differs")
        phases = ["before_sampling", *("before_attempt:" + row["attempt_id"] for row in attempts), "after_sampling"]
        if [row["phase"] for row in state["checks"]] != phases:
            errors.append("required identity checks missing or reordered")
        return errors
    except (KeyError, TypeError, ValueError):
        return ["declared tool qualification incomplete"]
