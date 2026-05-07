#!/usr/bin/env python3
"""
Render a Markdown diff between two benchmark JSON envelopes.

Usage:
    benchmarks/scripts/compare.py <baseline.json> <new.json> [--md OUT.md]

Reads two `ResultsEnvelope` JSONs (see `benchmarks/src/envelope.rs`)
produced by `make bench` / `scripts/run.sh`. Matches samples by
(workload, driver), computes the percent change in median latency,
and prints a Markdown table to stdout (or `--md OUT.md`).

Q8 commitments enforced:
  - same `workload` (W{n}.v{v}) on both sides — different versions
    aren't comparable; cross-version pairs are listed in their own
    "ignored" section with a reason.
  - same `driver` on both sides — different drivers measure
    different engines and aren't directly comparable here.

Cross-host warnings (different `host.cpu` or `host.os_kind`) are
non-fatal but flagged at the top.

Pure stdlib Python — no third-party deps. Runs on the same Python
that the project already uses for `scripts/bump-version.sh` etc.
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any


@dataclass(frozen=True)
class Sample:
    workload: str
    driver: str
    median_ns: float
    ops_per_s: float


def load_envelope(path: Path) -> dict[str, Any]:
    with path.open() as f:
        return json.load(f)


def index_samples(env: dict[str, Any]) -> dict[tuple[str, str], Sample]:
    out: dict[tuple[str, str], Sample] = {}
    for s in env["samples"]:
        key = (s["workload"], s["driver"])
        out[key] = Sample(
            workload=s["workload"],
            driver=s["driver"],
            median_ns=float(s["median_ns"]),
            ops_per_s=float(s["ops_per_s"]),
        )
    return out


def fmt_ns(ns: float) -> str:
    """Render a duration in the unit that produces 3-4 significant digits."""
    if ns >= 1e9:
        return f"{ns / 1e9:.3f} s"
    if ns >= 1e6:
        return f"{ns / 1e6:.3f} ms"
    if ns >= 1e3:
        return f"{ns / 1e3:.2f} µs"
    return f"{ns:.0f} ns"


def fmt_pct(baseline: float, new: float) -> str:
    """Percent change with sign. Positive = slower; negative = faster."""
    if baseline <= 0:
        return "—"
    delta = (new - baseline) / baseline * 100.0
    sign = "+" if delta >= 0 else ""
    return f"{sign}{delta:.1f}%"


def fmt_ratio(baseline: float, new: float) -> str:
    """Multiplicative ratio new/baseline, rendered as `1.42x` etc."""
    if baseline <= 0:
        return "—"
    return f"{new / baseline:.2f}x"


def host_summary(env: dict[str, Any]) -> str:
    h = env.get("host", {})
    return f"{h.get('cpu', '?')} / {h.get('os_kind', '?')} {h.get('arch', '?')}"


def commit_summary(env: dict[str, Any]) -> str:
    c = env.get("commit", {})
    sha = c.get("sha", "?")
    short = sha[:8] if len(sha) >= 8 else sha
    branch = c.get("branch", "?")
    dirty = " (dirty)" if c.get("dirty") else ""
    return f"{branch}@{short}{dirty}"


def render(baseline: dict[str, Any], new: dict[str, Any]) -> str:
    a = index_samples(baseline)
    b = index_samples(new)

    out: list[str] = []
    out.append("# Bench comparison\n")

    # Top-level envelope summary.
    out.append("## Envelopes\n")
    out.append("| Field | Baseline | New |")
    out.append("|---|---|---|")
    out.append(
        f"| Run started at | {baseline.get('run_started_at', '?')} | {new.get('run_started_at', '?')} |"
    )
    out.append(f"| Host | {host_summary(baseline)} | {host_summary(new)} |")
    out.append(
        f"| Commit | {commit_summary(baseline)} | {commit_summary(new)} |"
    )
    out.append(f"| Samples | {len(a)} | {len(b)} |")
    out.append("")

    # Cross-host warning.
    if baseline.get("host", {}).get("cpu") != new.get("host", {}).get("cpu") or baseline.get(
        "host", {}
    ).get("os_kind") != new.get("host", {}).get("os_kind"):
        out.append(
            "> ⚠️ **Cross-host comparison.** Baseline + new ran on different hosts; numbers aren't directly comparable. Treat ratios as directional only.\n"
        )

    # The diff table — keys present on both sides.
    common = sorted(a.keys() & b.keys())
    if common:
        out.append("## Diff\n")
        out.append(
            "| Workload | Driver | Baseline | New | Δ | Ratio (new/baseline) |"
        )
        out.append("|---|---|---|---|---|---|")
        for key in common:
            sa, sb = a[key], b[key]
            out.append(
                f"| `{sa.workload}` | {sa.driver} | {fmt_ns(sa.median_ns)} | {fmt_ns(sb.median_ns)} | {fmt_pct(sa.median_ns, sb.median_ns)} | {fmt_ratio(sa.median_ns, sb.median_ns)} |"
            )
        out.append("")

    # Asymmetries — keys present on only one side. Often this is a
    # workload-version bump (W1.v1 vs W1.v2) or a new workload landing.
    only_a = sorted(a.keys() - b.keys())
    only_b = sorted(b.keys() - a.keys())
    if only_a or only_b:
        out.append("## Only on one side\n")
        out.append("Likely a workload-version bump or a new / removed workload.\n")
        if only_a:
            out.append("**Only in baseline:**\n")
            for k in only_a:
                out.append(f"- `{k[0]}` / {k[1]} ({fmt_ns(a[k].median_ns)})")
            out.append("")
        if only_b:
            out.append("**Only in new:**\n")
            for k in only_b:
                out.append(f"- `{k[0]}` / {k[1]} ({fmt_ns(b[k].median_ns)})")
            out.append("")

    return "\n".join(out)


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("baseline", type=Path, help="Baseline JSON envelope (the 'before').")
    p.add_argument("new", type=Path, help="New JSON envelope (the 'after').")
    p.add_argument(
        "--md",
        type=Path,
        default=None,
        help="Write the report to this file instead of stdout.",
    )
    args = p.parse_args()

    if not args.baseline.exists():
        print(f"baseline file not found: {args.baseline}", file=sys.stderr)
        return 1
    if not args.new.exists():
        print(f"new file not found: {args.new}", file=sys.stderr)
        return 1

    baseline = load_envelope(args.baseline)
    new = load_envelope(args.new)

    # Schema-version sanity check.
    if baseline.get("schema_version") != new.get("schema_version"):
        print(
            f"schema-version mismatch: baseline={baseline.get('schema_version')} new={new.get('schema_version')}; aborting",
            file=sys.stderr,
        )
        return 2

    report = render(baseline, new)
    if args.md is not None:
        args.md.write_text(report)
        print(f"wrote {args.md}")
    else:
        print(report)
    return 0


if __name__ == "__main__":
    sys.exit(main())
