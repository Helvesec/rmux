"""Build AST-only specimens against the complete digest-bound frozen neighborhood."""
import ast
import hashlib
import json
from pathlib import Path

import pin_revisions as revisions
import provenance_archive as archive

ROOT = Path(__file__).resolve().parents[1]


def historical_source():
    proof = json.loads(revisions.proof_bytes(ROOT))
    item = next(row for row in proof["inventory"] if row["artifact"] == "inputs/historical-source.py.txt")
    raw = archive.archived_bytes({"schema": archive.SCHEMA,
        **{key: item[key] for key in ("git_blob", "bytes", "sha256")}}, ROOT)
    population = json.loads(archive.read(ROOT / "benches/perf/qualification/population.json", ROOT))
    if hashlib.sha256(raw).hexdigest() != population["source_sha256"]:
        raise ValueError("historical source differs from frozen population")
    return raw.decode()


def revised_source(*names):
    source = historical_source()
    specimen = (ROOT / "tests/fixtures/pin_revision_definitions.py").read_text()
    replacements = {node.name: ast.get_source_segment(specimen, node)
                    for node in ast.parse(specimen).body if isinstance(node, ast.FunctionDef)}
    lines = source.splitlines(keepends=True)
    # Replace only authorized definitions, retaining every other frozen AST.
    for node in reversed(ast.parse(source).body):
        if isinstance(node, ast.FunctionDef) and node.name in names:
            if node.name not in revisions.ALLOWED:
                raise ValueError("specimen outside D147/D152")
            lines[node.lineno - 1:node.end_lineno] = [replacements[node.name] + "\n"]
    return "".join(lines)
