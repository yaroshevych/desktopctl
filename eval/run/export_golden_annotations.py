#!/usr/bin/env python3
"""Export human annotations from a Label Studio project into a run folder.

Reads task_completion.result from the LS postgres DB, converts percent-based
rectanglelabels to pixel bboxes, and writes:
  - datasets/runs/<run_id>/artifacts.csv
  - datasets/runs/<run_id>/gt_labels/<label_id>/label.json  (GT in tokenizer schema)

The artifacts.csv label_path column points to these GT label files (not the
original Screen2AX/vm label files), so the strict evaluator can compare
tokenizer output against fresh human labels.

Usage:
    uv run run/export_golden_annotations.py --project-id 6 --slug golden_tf
    uv run run/export_golden_annotations.py --project-id 6 --slug golden_tf --label-filter text_field
"""

from __future__ import annotations

import argparse
import csv
import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path

import psycopg2
from dotenv import load_dotenv

load_dotenv(Path(__file__).parent.parent / ".env")

DATASETS_DIR = Path(__file__).parent.parent / "datasets"


def ls_url_to_path(url: str, data_dir: Path) -> Path:
    rel = url.replace("/data/local-files/?d=", "")
    return data_dir / rel


def convert_rect(value: dict) -> list[float]:
    """Convert LS percent-based rect to pixel [x, y, w, h]."""
    ow = value["original_width"]
    oh = value["original_height"]
    return [
        value["x"] * ow / 100.0,
        value["y"] * oh / 100.0,
        value["width"] * ow / 100.0,
        value["height"] * oh / 100.0,
    ]


def fetch_tasks(conn, project_id: int) -> list[dict]:
    cur = conn.cursor()
    cur.execute("""
        SELECT t.id, t.data, tc.result
        FROM task t
        JOIN task_completion tc ON tc.task_id = t.id
        WHERE t.project_id = %s
        ORDER BY t.id
    """, (project_id,))
    rows = []
    for task_id, data, result in cur.fetchall():
        rows.append({"task_id": task_id, "data": data, "result": result})
    return rows


def build_gt_label(
    task: dict,
    data_dir: Path,
    label_filter: str | None,
) -> dict | None:
    """Build a GT label.json (tokenizer schema) from LS annotations."""
    data = task["data"]
    result = task["result"]

    image_url = data.get("image", "")
    if not image_url:
        return None
    image_path = ls_url_to_path(image_url, data_dir)

    # Collect rectanglelabels matching the filter
    elements = []
    el_idx = 1
    for ann in result:
        if ann.get("type") != "rectanglelabels":
            continue
        value = ann.get("value", {})
        labels = value.get("rectanglelabels", [])
        if label_filter and label_filter not in labels:
            continue
        bbox = convert_rect({**value, "original_width": ann["original_width"], "original_height": ann["original_height"]})
        category = labels[0] if labels else "unknown"
        elements.append({
            "id": f"gt_{el_idx:04d}",
            "type": "box",
            "bbox": bbox,
            "category": category,
            "source": f"ls_human:{category}",
        })
        el_idx += 1

    if not elements:
        return None

    # Get image dimensions from first annotation original_width/height
    first = result[0] if result else {}
    width = first.get("original_width", 0)
    height = first.get("original_height", 0)

    return {
        "image": {
            "path": str(image_path),
            "width": width,
            "height": height,
        },
        "windows": [
            {
                "id": "win_0001",
                "elements": elements,
            }
        ],
        "meta": {
            "dataset": data.get("dataset", ""),
            "control_type": data.get("control_type", ""),
            "ls_task_id": task["task_id"],
            "label_filter": label_filter,
        },
    }


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--project-id", type=int, required=True, help="Label Studio project ID")
    p.add_argument("--slug", required=True, help="Short name for this run")
    p.add_argument("--label-filter", default="text_field",
                   help="Only export annotations with this rectanglelabel (default: text_field, '' for all)")
    p.add_argument("--pg-host", default="localhost")
    p.add_argument("--pg-port", type=int, default=5432)
    p.add_argument("--pg-user", default=os.getenv("POSTGRES_USER", "desktopctl"))
    p.add_argument("--pg-password", default=os.getenv("POSTGRES_PASSWORD"))
    p.add_argument("--data-dir", default=os.getenv("DATA_DIR"))
    return p.parse_args()


def main() -> None:
    args = parse_args()

    if not args.pg_password:
        sys.exit("POSTGRES_PASSWORD not set")
    if not args.data_dir:
        sys.exit("DATA_DIR not set")

    data_dir = Path(args.data_dir).resolve()
    label_filter = args.label_filter or None
    run_id = datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S") + "-" + args.slug
    run_dir = DATASETS_DIR / "runs" / run_id
    gt_dir = run_dir / "gt_labels"
    run_dir.mkdir(parents=True, exist_ok=True)

    conn = psycopg2.connect(
        host=args.pg_host, port=args.pg_port,
        dbname="labelstudio",
        user=args.pg_user, password=args.pg_password,
    )

    print(f"Fetching tasks from project #{args.project_id}...")
    tasks = fetch_tasks(conn, args.project_id)
    print(f"  found {len(tasks)} tasks with completions")

    csv_path = run_dir / "artifacts.csv"
    fieldnames = ["label_id", "sample_id", "control_type", "dataset",
                  "image_path", "overlay_path", "label_path", "metadata_path"]

    exported = 0
    skipped = 0

    with csv_path.open("w", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=fieldnames)
        writer.writeheader()

        for task in tasks:
            data = task["data"]
            label_id = data.get("label_id", f"task_{task['task_id']}")
            image_url = data.get("image", "")
            image_path = ls_url_to_path(image_url, data_dir)

            gt_label = build_gt_label(task, data_dir, label_filter)
            if gt_label is None:
                print(f"  SKIP {label_id} (no matching annotations)")
                skipped += 1
                continue

            # Write GT label.json
            gt_label_dir = gt_dir / label_id
            gt_label_dir.mkdir(parents=True, exist_ok=True)
            gt_label_path = gt_label_dir / "label.json"
            gt_label_path.write_text(json.dumps(gt_label, indent=2))

            writer.writerow({
                "label_id":      label_id,
                "sample_id":     data.get("sample_id", ""),
                "control_type":  data.get("control_type", ""),
                "dataset":       data.get("dataset", ""),
                "image_path":    image_path,
                "overlay_path":  "",
                "label_path":    gt_label_path,
                "metadata_path": "",
            })
            exported += 1

    (run_dir / "meta.json").write_text(json.dumps({
        "run_id": run_id,
        "ls_project_id": args.project_id,
        "slug": args.slug,
        "label_filter": label_filter,
        "n_exported": exported,
        "n_skipped": skipped,
    }, indent=2))

    print(f"\n  exported: {exported}  skipped: {skipped}")
    print(f"  run_id:   {run_id}")
    print(f"  csv:      {csv_path}")
    print(f"\nnext steps:")
    print(f"  just run-tokenize {run_id}")
    print(f"  uv run run/eval_text_fields_strict.py --run-dir datasets/runs/{run_id}")


if __name__ == "__main__":
    main()
