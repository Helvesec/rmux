"""Web compatibility checks shared by existing release consumers."""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from web_compatibility_matrix import ROOT, lookup, validate_published


def validate_browser_contract() -> None:
    matrix = validate_published(ROOT)
    lookup(matrix, "frontend_daemon", frontend="web-v1", daemon="web-v1")
    artifacts = [row for row in matrix["artifacts"] if row["id"] == "wasm"]
    if len(artifacts) != 1 or artifacts[0]["kind"] != "crypto":
        raise ValueError("browser WASM artifact is absent from the Web compatibility catalogue")
