#!/usr/bin/env python3
"""Create a new run folder from a filtered subset of an existing run's annotations.

Reads annotations.csv from the source run, filters by verdict, and writes a new
artifacts.csv pointing to the source run's result files.

Usage:
    uv run run/subset_run.py --source 20260321-185915-text_fields --verdict follow_up --slug follow_up_v2
    uv run run/subset_run.py --source 20260321-185915-text_fields --verdict accept,follow_up --slug mixed_v2
"""

from __future__ import annotations

import argparse
import csv
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

RUNS_DIR = Path(__file__).parent.parent / "datasets" / "runs"


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--source", required=True, help="Source run ID")
    ap.add_argument("--verdict", required=True, help="Comma-separated verdicts to include, e.g. follow_up or accept,follow_up")
    ap.add_argument("--slug", required=True, help="Short name for the new run")
    args = ap.parse_args()

    source_dir = RUNS_DIR / Path(args.source).name
    annotations_path = source_dir / "annotations.csv"
    if not annotations_path.exists():
        sys.exit(f"annotations.csv not found in {source_dir}\nRun: just run-log {args.source}")

    verdicts = {v.strip() for v in args.verdict.split(",")}

    # Load original paths from source artifacts.csv
    artifacts_path = source_dir / "artifacts.csv"
    if not artifacts_path.exists():
        sys.exit(f"artifacts.csv not found in {source_dir}")
    artifacts_by_label: dict[str, dict] = {}
    with artifacts_path.open() as f:
        for row in csv.DictReader(f):
            artifacts_by_label[row["label_id"]] = row

    rows = []
    with annotations_path.open() as f:
        for row in csv.DictReader(f):
            if row["verdict"] not in verdicts:
                continue
            label_id = row["label_id"]
            artifact = artifacts_by_label.get(label_id)
            if not artifact:
                print(f"  warning: {label_id} not in artifacts.csv, skipping")
                continue
            rows.append({
                "label_id":      label_id,
                "sample_id":     row["sample_id"],
                "control_type":  row["control_type"],
                "dataset":       row["dataset"],
                "image_path":    artifact["image_path"],
                "overlay_path":  artifact["overlay_path"],
                "label_path":    artifact["label_path"],
                "metadata_path": artifact["metadata_path"],
                "verdict":       row["verdict"],
                "comment":       row["comment"],
            })

    if not rows:
        sys.exit(f"no rows matched verdict(s): {args.verdict}")

    run_id = datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S") + "-" + args.slug
    run_dir = RUNS_DIR / run_id
    run_dir.mkdir(parents=True, exist_ok=True)

    csv_path = run_dir / "artifacts.csv"
    fieldnames = ["label_id", "sample_id", "control_type", "dataset",
                  "image_path", "overlay_path", "label_path", "metadata_path",
                  "verdict", "comment"]
    with csv_path.open("w", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=fieldnames)
        writer.writeheader()
        writer.writerows(rows)

    (run_dir / "meta.json").write_text(json.dumps({
        "run_id": run_id,
        "source_run": args.source,
        "verdicts": sorted(verdicts),
        "slug": args.slug,
        "n_artifacts": len(rows),
    }, indent=2))

    print(f"  source:  {args.source}")
    print(f"  verdicts: {args.verdict}")
    print(f"  rows:    {len(rows)}")
    print(f"  run_id:  {run_id}")
    print(f"  written: {csv_path}")
    print(f"\nnext steps:")
    print(f"  just run-tokenize {run_id}")
    print(f"  just run-import   {run_id}")
    print(f"  just run-log      {run_id}")


if __name__ == "__main__":
    main()
