"""Attribution for the three Rust 1.88 diagnostic forms used by our canaries."""

import json
from pathlib import Path


def wire_refusal_diagnostic(client, daemon, endpoint, fixture=False):
    """Pin the start-server diagnostic to the admitted v8/v9 package pair."""
    versions = {8: "0.10.0", 9: "0.11.0"}
    for package in (client, daemon):
        wire = package.get("wire")
        if type(wire) is not int or versions.get(wire) != package.get("version"):
            raise ValueError("refusal requires the selected installed version/wire pair")
    if client["wire"] == daemon["wire"] or type(fixture) is not bool:
        raise ValueError("refusal requires distinct installed wire versions")
    if fixture:
        message = (f"rmux: incompatible protocol {daemon['wire']}; "
                   f"client supports protocol {client['wire']}.")
        quoted = endpoint
    else:
        if (client["wire"], daemon["wire"]) == (8, 9):
            # The published v8 client predates the numbered refusal diagnostic.
            message = f"rmux: running daemon on '{endpoint}' uses an incompatible protocol."
        else:
            message = (f"rmux: running daemon from an older release uses incompatible protocol {daemon['wire']}; "
                       f"this client supports protocol {client['wire']} on '{endpoint}'.")
        # Match rmux-client's shell_quote_path, including Windows pipe names.
        safe = endpoint and all(c.isascii() and (c.isalnum() or c in "/._-+=:@") for c in endpoint)
        quoted = endpoint if safe else "'" + endpoint.replace("'", "'\\''") + "'"
    return message + f"\nrmux: run `rmux -S {quoted} kill-server` to stop it, then retry."


def validate_wire_refusal(record):
    """Accept only an observed refusal with an explicit installed restart path.

    Package membership is checked on disk by the migration collector. This
    predicate checks its receipt; it does not authenticate arbitrary JSON or
    substitute for release signatures and native campaign qualification.
    """
    client = record.get("client", {})
    command = record.get("command", [])
    if (not client.get("path") or client.get("path") != client.get("installed_path")
            or not client.get("sha256") or client.get("sha256") != client.get("installed_sha256")
            or not command or command[0] != client["path"]):
        raise ValueError("refusal executable differs from installed package")
    code = record.get("returncode")
    expected = record.get("expected_returncode")
    stderr = record.get("stderr", "")
    diagnostic = record.get("diagnostic", "")
    restart = record.get("restart", {})
    endpoint = command[2] if len(command) >= 4 and command[1] == "-S" else None
    if not isinstance(endpoint, str) or not endpoint:
        raise ValueError("refusal requires its explicit private endpoint")
    daemon = restart.get("client", {})
    if "wire" in client or "wire" in daemon:
        exact = wire_refusal_diagnostic(client, daemon, endpoint, record.get("fixture", False))
        declared = exact
    else:
        # Frozen, versionless unit receipt; installed calls always carry wires.
        declared = "incompatible protocol"
        exact = (f"rmux: incompatible protocol; run old rmux -S {endpoint} kill-server; "
                 "sessions will be lost")
    if (type(code) is not int or type(expected) is not int or expected != 1
            or code != expected or diagnostic != declared or not isinstance(stderr, str)
            or stderr.removesuffix("\n").removesuffix("\r") != exact):
        raise ValueError("refusal requires its exact exit code, diagnostic and restart advice")
    before = record.get("before")
    if (not isinstance(before, dict) or type(before.get("pid")) is not int or before["pid"] <= 0
            or not isinstance(before.get("sessions"), list)
            or record.get("after_refusal") != before):
        raise ValueError("refusal changed the daemon or its sessions")
    argv = restart.get("command", [])
    if (restart.get("explicit") is not True or type(restart.get("returncode")) is not int
            or restart["returncode"] != 0
            or restart.get("sessions_lost_acknowledged") is not True
            or not argv or argv[-1] != "kill-server" or command[1:2] != ["-S"]
            or argv[1:3] != command[1:3]):
        raise ValueError("refusal requires a successful explicit shutdown and loss acknowledgement")


def expected_diagnostic(code, item, source, package_id=None):
    if source is None:
        raise ValueError("a negative compiler control requires its expected source")
    messages = {
        "E0432": f"unresolved import `{item}`",
        "E0603": f"struct `{item}` is private",
        "E0583": f"file not found for module `{item}`",
    }
    return {"code": code, "message": messages[code],
            "source": str(Path(source).resolve()), "package_id": package_id}


def match_diagnostic(stdout, stderr, cwd, expected):
    """Never search rendered diagnostics, source echoes, notes or another stream.

    Cargo's compiler-message envelope identifies the package; standalone rustc
    writes diagnostic objects to stderr. A primary span must name the exact
    expected source. Additional diagnostics cannot supply parts of this match.
    """
    stream = stdout if expected["package_id"] is not None else stderr
    for line in stream.splitlines():
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(value, dict):
            continue
        if expected["package_id"] is not None:
            if (value.get("reason") != "compiler-message"
                    or value.get("package_id") != expected["package_id"]):
                continue
            value = value.get("message")
        elif value.get("$message_type") != "diagnostic":
            continue
        if not isinstance(value, dict) or value.get("level") != "error":
            continue
        code = value.get("code")
        if (not isinstance(code, dict) or code.get("code") != expected["code"]
                or value.get("message") != expected["message"]):
            continue
        spans = value.get("spans")
        if not isinstance(spans, list):
            continue
        for span in spans:
            if (isinstance(span, dict) and span.get("is_primary") is True
                    and isinstance(span.get("file_name"), str)
                    and str((Path(cwd) / span["file_name"]).resolve()) == expected["source"]):
                return {**expected, "line": span.get("line_start"),
                        "column": span.get("column_start")}
    return None
