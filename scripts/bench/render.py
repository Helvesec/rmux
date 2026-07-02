#!/usr/bin/env python3
"""Render public RMUX benchmark JSON artifacts to Markdown."""

from __future__ import annotations

import argparse
import csv
import datetime as dt
import html
import json
import math
import re
from io import StringIO
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parents[2]

TOOLS = {
    "rmux": "rmux",
    "tmux": "tmux",
    "psmux": "psmux",
    "zellij": "zellij",
}

PLATFORM_ORDER = {"linux": 0, "windows": 1, "macos": 2}

PREFERRED_TOOL_ORDER = {
    "linux": ["rmux", "tmux", "zellij"],
    "macos": ["rmux", "tmux", "zellij"],
    "windows": ["rmux", "tmux", "zellij", "psmux"],
}

PUBLIC_TABLE_HIDDEN_TOOLS = {
    "windows": {"psmux"},
}

SCENARIO_LABELS = {
    "list_commands": "List available commands",
    "new_session_cold_sh": "Create background session (cold daemon)",
    "new_session_warm_sh": "Create background session (default shell)",
    "split_window_h_detached_sh": "Split pane left/right",
    "split_window_v_detached_sh": "Split pane top/bottom",
    "split_window_h_attached_sh": "Split pane left/right, attached",
    "split_window_v_attached_sh": "Split pane top/bottom, attached",
    "new_window_detached_sh": "Create new tab/window",
    "new_window_then_kill": "Create and close new tab/window",
    "send_keys_detached_round_trip": "Send keys to pane/window",
    "capture_pane_80x24": "Capture visible output",
    "capture_pane_5000_lines": "Capture 5,000 lines of scrollback",
    "capture_pane_200x50_scrollback_10k": "Capture full scrollback",
    "list_sessions_default": "List sessions",
    "list_windows_20": "List tabs/windows",
    "list_panes_80": "List panes",
    "resize_pane_absolute_100x30": "Resize pane to 100x30",
    "resize_pane_absolute_200x50": "Resize pane to 200x50",
    "resize_pane_right_1": "Grow pane by 1 cell",
    "resize_pane_right_10": "Grow pane by 10 cells",
    "resize_pane_left_1": "Shrink pane by 1 cell",
    "display_message_default": "Display message",
    "show_options_global": "Show global options",
    "show_window_options": "Show window options",
    "rename_window": "Rename window",
    "select_window_next": "Select next window",
    "join_pane_detached": "Join pane",
    "source_file_minimal": "Source minimal config",
    "set_option_quiet": "Set global option",
    "set_window_option_quiet": "Set window option",
    "kill_pane": "Close pane",
    "kill_session": "Terminate session",
    "kill_server": "Kill server",
}

VISIBLE_OPERATION_ORDER = [
    "new_session_warm_sh",
    "list_sessions_default",
    "split_window_h_detached_sh",
    "split_window_v_detached_sh",
    "send_keys_detached_round_trip",
    "capture_pane_80x24",
    "capture_pane_200x50_scrollback_10k",
    "list_windows_20",
    "resize_pane_right_1",
    "kill_session",
]

PLATFORM_ICON = {
    "linux": ("install/linux.svg", "install/linux-light.svg"),
    "macos": ("install/apple.svg", "install/apple-light.svg"),
    "windows": ("install/windows.svg", "install/windows-light.svg"),
}

HERO_ASSET_STEM = "hero-v0.8.0-rmux"

DARK = {
    "bg": "#05101b",
    "bg2": "#071827",
    "line": "#263848",
    "muted": "#aeb8c6",
    "text": "#f3f7fb",
    "panel": "#081522",
}

LIGHT = {
    "bg": "#f5fbff",
    "bg2": "#eaf5ff",
    "line": "#c8d8e8",
    "muted": "#526170",
    "text": "#13202d",
    "panel": "#ffffff",
}


def now_iso() -> str:
    return dt.datetime.now(dt.UTC).replace(microsecond=0).isoformat().replace("+00:00", "Z")


def load_payloads(paths: list[Path]) -> list[dict[str, Any]]:
    payloads: list[dict[str, Any]] = []
    for path in paths:
        with path.open("r", encoding="utf-8-sig") as handle:
            loaded = json.load(handle)
        source = {
            "_source_name": path.name,
            "_source_mtime": path.stat().st_mtime,
        }
        if isinstance(loaded, list):
            for item in loaded:
                if isinstance(item, dict):
                    item.update(source)
                    payloads.append(item)
        else:
            loaded.update(source)
            payloads.append(loaded)
    return payloads


def payload_source_priority(payload: dict[str, Any]) -> tuple[int, float]:
    platform = platform_id(payload).lower()
    source_name = str(payload.get("_source_name", "")).lower()
    source_mtime = float(payload.get("_source_mtime", 0.0))
    completeness_rank = 0 if payload.get("complete", True) else 3
    if source_name == f"{platform}.json":
        return (completeness_rank, -source_mtime)
    if "smoke" in source_name:
        return (completeness_rank + 2, -source_mtime)
    return (completeness_rank + 1, -source_mtime)


def select_canonical_payloads(payloads: list[dict[str, Any]]) -> list[dict[str, Any]]:
    by_platform: dict[str, list[dict[str, Any]]] = {}
    for payload in payloads:
        by_platform.setdefault(platform_id(payload), []).append(payload)
    selected = []
    for candidates in by_platform.values():
        selected.append(sorted(candidates, key=payload_source_priority)[0])
    return selected


def metric_value(op: dict[str, Any], tool: str) -> dict[str, float] | None:
    value = op.get("metrics", {}).get(tool)
    if not isinstance(value, dict) or "p50_ms" not in value:
        return None
    return {"p50_ms": float(value["p50_ms"]), "p95_ms": float(value.get("p95_ms", value["p50_ms"]))}


def format_ms(value: dict[str, float] | None) -> str:
    if value is None or math.isnan(value["p50_ms"]):
        return "-"
    return f"{value['p50_ms']:.3f} ms"


def ratio_text(op: dict[str, Any], baseline: str) -> str:
    rmux = metric_value(op, "rmux")
    base = metric_value(op, baseline)
    if rmux is None or base is None or rmux["p50_ms"] <= 0:
        return "-"
    ratio = base["p50_ms"] / rmux["p50_ms"]
    if 0.80 <= ratio <= 1.25:
        return "≈ same speed"
    if ratio < 1:
        return f"{1 / ratio:.1f}x slower"
    return f"{ratio:.1f}x faster"


def ratio_badge(op: dict[str, Any], baseline: str) -> str:
    value = ratio_text(op, baseline)
    if value == "-":
        return "-"
    if value.startswith("≈"):
        return f"<kbd>{html.escape(value)}</kbd>"
    marker = "🟢" if value.endswith("faster") else "🔴"
    return f"<kbd>{marker} {html.escape(value)}</kbd>"


def platform_id(payload: dict[str, Any]) -> str:
    return str(payload.get("platform", {}).get("id", ""))


def ordered_tools(payload: dict[str, Any]) -> list[str]:
    available = [str(tool) for tool in payload.get("tools", []) if str(tool) in TOOLS]
    preferred = PREFERRED_TOOL_ORDER.get(platform_id(payload), [])
    ordered = [tool for tool in preferred if tool in available]
    ordered.extend(tool for tool in available if tool not in ordered)
    return ordered


def ordered_public_tools(payload: dict[str, Any]) -> list[str]:
    hidden = PUBLIC_TABLE_HIDDEN_TOOLS.get(platform_id(payload), set())
    return [tool for tool in ordered_tools(payload) if tool not in hidden]


def baseline_tool(payload: dict[str, Any]) -> str:
    tools = ordered_tools(payload)
    if "tmux" in tools:
        return "tmux"
    return str(payload.get("baseline", "tmux"))


def tool_label(tool: str, platform: str) -> str:
    if platform == "windows" and tool == "tmux":
        return f"{TOOLS.get(tool, tool)} (WSL)"
    return TOOLS.get(tool, tool)


def picture(dark: str, light: str, alt: str, width: str = "100%") -> str:
    return (
        "<picture>\n"
        f'  <source media="(prefers-color-scheme: dark)" srcset="{dark}">\n'
        f'  <img src="{light}" width="{width}" alt="{html.escape(alt)}">\n'
        "</picture>"
    )


def platform_title(platform_id: str, name: str) -> str:
    dark, light = PLATFORM_ICON.get(platform_id, ("", ""))
    icon = picture(dark, light, name, "32") if dark else ""
    return f'<h2 align="center">{icon}<br>{html.escape(name)}</h2>'


def scenario_label(operation_id: str) -> str:
    return SCENARIO_LABELS.get(operation_id, operation_id.replace("_", " "))


def operation_rows(payload: dict[str, Any], operations: list[dict[str, Any]]) -> str:
    tools = ordered_public_tools(payload)
    baseline = baseline_tool(payload)
    rows = []
    for op in operations:
        operation_id = str(op.get("id", op.get("label", "")))
        cells = [f"<td>{html.escape(scenario_label(operation_id))}</td>"]
        for tool in tools:
            cells.append(f"<td align=\"right\"><code>{format_ms(metric_value(op, tool))}</code></td>")
        cells.append(f"<td align=\"right\"><strong>{ratio_badge(op, baseline)}</strong></td>")
        rows.append("<tr>" + "".join(cells) + "</tr>")
    return "\n".join(rows)


def table_header(payload: dict[str, Any]) -> str:
    tools = ordered_public_tools(payload)
    platform = platform_id(payload)
    baseline = baseline_tool(payload)
    cells = ["<th align=\"left\">Scenario</th>"]
    cells.extend(f"<th align=\"right\">{html.escape(tool_label(tool, platform))}</th>" for tool in tools)
    cells.append(f"<th align=\"right\">vs {html.escape(tool_label(baseline, platform))}</th>")
    return "<tr>" + "".join(cells) + "</tr>"


def operation_table(payload: dict[str, Any], operations: list[dict[str, Any]]) -> str:
    return (
        '<div align="center">\n\n'
        '<table align="center">\n'
        "<thead>\n"
        f"{table_header(payload)}\n"
        "</thead>\n"
        "<tbody>\n"
        f"{operation_rows(payload, operations)}\n"
        "</tbody>\n"
        "</table>\n\n"
        "</div>"
    )


def csv_name(payload: dict[str, Any]) -> str:
    platform = re.sub(r"[^a-z0-9_-]+", "-", platform_id(payload).lower()).strip("-")
    return f"{platform or 'benchmark'}.csv"


def raw_link(payload: dict[str, Any], asset_dir: Path) -> str:
    return f"{asset_dir.name}/{csv_name(payload)}"


def visible_operations(operations: list[dict[str, Any]], summary_rows: int) -> list[dict[str, Any]]:
    by_id = {str(operation.get("id", "")): operation for operation in operations}
    visible = [
        by_id[operation_id]
        for operation_id in VISIBLE_OPERATION_ORDER
        if operation_id in by_id
    ]
    return visible[:summary_rows]


def platform_section(payload: dict[str, Any], summary_rows: int, asset_dir: Path) -> str:
    platform = payload["platform"]
    platform_id = str(platform["id"])
    operations = list(payload["operations"])
    visible = visible_operations(operations, summary_rows)
    commit = str(payload.get("git", {}).get("commit", ""))[:12]
    title_note = "p50 latency, lower is faster"
    if commit and commit != "unknown" and not payload.get("draft"):
        title_note = f"{commit} · {title_note}"
    title_note = (
        f'{html.escape(title_note)}. '
        f'<a href="{html.escape(raw_link(payload, asset_dir))}">Full Raw Benchmarks</a>'
    )
    note_line = ""
    if platform_id == "macos":
        note_line += (
            "\n<p align=\"center\"><sub>macOS note: measured on a MacBook Pro. "
            "Most operations finish around 5 ms on this machine; read those rows "
            "as \"same speed\".</sub></p>\n"
        )
    return (
        f'<a id="{html.escape(platform_id)}"></a>\n'
        f"{platform_title(platform_id, str(platform['name']))}\n\n"
        f'<p align="center"><sub>{title_note}</sub></p>\n'
        f"{note_line}\n"
        f"{operation_table(payload, visible)}\n"
    )


def render_hero(mode: str) -> str:
    colors = DARK if mode == "dark" else LIGHT
    logo = logo_markup(mode)
    return f"""<svg xmlns="http://www.w3.org/2000/svg" width="1500" height="156" viewBox="0 0 1500 156" role="img" aria-label="RMUX benchmark overview">
  <defs>
    <linearGradient id="bg" x1="0" x2="1" y1="0" y2="1">
      <stop offset="0" stop-color="{colors['bg']}"/>
      <stop offset="1" stop-color="{colors['bg2']}"/>
    </linearGradient>
  </defs>
  <rect x="3" y="3" width="1494" height="150" rx="14" fill="url(#bg)" stroke="{colors['line']}"/>
  {logo}
  <text x="145" y="61" fill="{colors['text']}" font-family="Inter, ui-sans-serif, system-ui, sans-serif" font-size="43" font-weight="800">Benchmark</text>
  <text x="145" y="96" fill="{colors['muted']}" font-family="Inter, ui-sans-serif, system-ui, sans-serif" font-size="21">Performance comparison between RMUX and tmux across common operations.</text>
  <text x="145" y="129" fill="#7dff58" font-family="Inter, ui-sans-serif, system-ui, sans-serif" font-size="21">Lower is faster.</text>
  <rect x="1268" y="38" width="205" height="54" rx="15" fill="{colors['panel']}" stroke="{colors['line']}"/>
  <text x="1370" y="72" fill="{colors['text']}" text-anchor="middle" font-family="Inter, ui-sans-serif, system-ui, sans-serif" font-size="20" font-weight="700">RMUX v0.8.0</text>
</svg>
"""


def logo_markup(mode: str) -> str:
    logo_path = REPO_ROOT / "docs" / f"rmux-logo-{mode}.svg"
    source = logo_path.read_text(encoding="utf-8")
    match = re.search(r"<svg[^>]*viewBox=\"([^\"]+)\"[^>]*>(.*)</svg>", source, re.S)
    if not match:
        return ""
    view_box = [float(part) for part in match.group(1).split()]
    body = match.group(2).strip()
    _, _, logo_width, logo_height = view_box
    box_x, box_y, box_width, box_height = 22.0, 22.0, 94.0, 86.0
    scale = min(box_width / logo_width, box_height / logo_height)
    dx = box_x + (box_width - logo_width * scale) / 2.0
    dy = box_y + (box_height - logo_height * scale) / 2.0
    return f'<g transform="translate({dx:.3f} {dy:.3f}) scale({scale:.6f})">{body}</g>'


def write_static_assets(asset_dir: Path) -> None:
    asset_dir.mkdir(parents=True, exist_ok=True)
    for mode in ("dark", "light"):
        (asset_dir / f"{HERO_ASSET_STEM}-{mode}.svg").write_text(
            render_hero(mode), encoding="utf-8"
        )


def csv_text(payload: dict[str, Any]) -> str:
    handle = StringIO()
    writer = csv.writer(handle, lineterminator="\n")
    writer.writerow(
        [
            "platform",
            "commit",
            "operation",
            "scenario",
            "tool",
            "sample_index",
            "sample_ms",
            "p50_ms",
            "p95_ms",
        ]
    )
    platform = platform_id(payload)
    commit = str(payload.get("git", {}).get("commit", ""))
    for operation in payload.get("operations", []):
        op_id = str(operation.get("id", operation.get("label", "")))
        scenario = scenario_label(op_id)
        metrics = operation.get("metrics", {})
        if not isinstance(metrics, dict):
            continue
        for tool in ordered_tools(payload):
            metric = metrics.get(tool)
            if not isinstance(metric, dict):
                continue
            samples = metric.get("samples_ms", [])
            if not isinstance(samples, list):
                samples = []
            p50 = metric.get("p50_ms", "")
            p95 = metric.get("p95_ms", "")
            for index, sample in enumerate(samples):
                writer.writerow([platform, commit, op_id, scenario, tool, index, sample, p50, p95])
    return handle.getvalue()


def write_csv_assets(payloads: list[dict[str, Any]], asset_dir: Path) -> None:
    asset_dir.mkdir(parents=True, exist_ok=True)
    for payload in payloads:
        (asset_dir / csv_name(payload)).write_text(csv_text(payload), encoding="utf-8")


def methodology(generated: str, commit: str) -> str:
    return f"""## Reproducing

Run the scripts locally on each OS, gather the JSON files, and generate the Markdown:

```bash
scripts/bench/run-unix.sh --out target/benchmarks/linux.json
scripts/bench/run-windows.ps1 -Out target/benchmarks/windows.json
scripts/bench/run-unix.sh --out target/benchmarks/macos.json
python3 scripts/bench/render.py target/benchmarks/*.json --output docs/benchmarks.md
```

Note: Scripts run safely in isolated sessions and only kill their own test processes.

## Methodology

The `render.py` script updates this Markdown file and linked CSVs based on the JSON results.

`-`: Operation is unavailable or not comparable.

Zellij: Results only show exact equivalents or close approximations.

Generated at `{generated}` from commit `{commit}`.
"""


def benchmark_commit(payloads: list[dict[str, Any]]) -> str:
    commits = {
        str(payload.get("git", {}).get("commit", "")).strip()
        for payload in payloads
        if str(payload.get("git", {}).get("commit", "")).strip()
    }
    if len(commits) == 1:
        return next(iter(commits))[:12]
    if commits:
        return "mixed artifacts"
    return "unknown"


def markdown(payloads: list[dict[str, Any]], asset_dir: Path, summary_rows: int) -> str:
    rel = asset_dir.name
    sections = "\n\n".join(
        platform_section(payload, summary_rows, asset_dir) for payload in payloads
    )
    generated = now_iso()
    commit = benchmark_commit(payloads)
    return f"""<!-- Generated by scripts/bench/render.py; edit the renderer or benchmark JSON inputs. -->
<div align="center">

{picture(f"{rel}/{HERO_ASSET_STEM}-dark.svg", f"{rel}/{HERO_ASSET_STEM}-light.svg", "RMUX benchmark overview")}

</div>

{sections}

{methodology(generated, commit)}
"""


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inputs", nargs="*", type=Path, help="benchmark JSON artifacts")
    parser.add_argument("--output", type=Path, default=Path("docs/benchmarks.md"))
    parser.add_argument("--asset-dir", type=Path, default=Path("docs/benchmarks"))
    parser.add_argument("--summary-rows", type=int, default=10)
    parser.add_argument("--write-assets", action="store_true", help="rewrite static hero SVG assets")
    args = parser.parse_args()

    if args.write_assets:
        write_static_assets(args.asset_dir)

    if not args.inputs:
        if args.write_assets:
            return 0
        parser.error("pass benchmark JSON files")

    payloads = select_canonical_payloads(load_payloads(args.inputs))
    payloads.sort(key=lambda payload: PLATFORM_ORDER.get(payload["platform"]["id"], 99))
    write_csv_assets(payloads, args.asset_dir)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        markdown(payloads, args.asset_dir, args.summary_rows).rstrip() + "\n",
        encoding="utf-8",
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
