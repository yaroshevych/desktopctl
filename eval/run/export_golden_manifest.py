#!/usr/bin/env python3
"""Export human annotations from Label Studio project 6 (Golden Set) into
a manifest.json suitable for the Rust golden_labels integration test.

Connects to the LS postgres DB, fetches all tasks + completions from project 6,
converts percent-based rectanglelabels to pixel [x, y, w, h] bboxes, resolves
absolute image paths, and writes:
  src/desktop/daemon/tests/fixtures/golden/manifest.json

Usage:
    uv run run/export_golden_manifest.py
    uv run run/export_golden_manifest.py --project-id 6
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

import psycopg2
from dotenv import load_dotenv

load_dotenv(Path(__file__).parent.parent / ".env")

REPO_ROOT = Path(__file__).parent.parent.parent
DATA_DIR = Path(os.getenv("DATA_DIR", str(Path(__file__).parent.parent / "datasets")))
MANIFEST_PATH = REPO_ROOT / "src/desktop/daemon/tests/fixtures/golden/manifest.json"


def ls_url_to_path(url: str, data_dir: Path) -> Path:
    rel = url.replace("/data/local-files/?d=", "")
    return data_dir / rel


def convert_rect(ann: dict) -> list[float]:
    """Convert LS percent-based rect annotation to pixel [x, y, w, h]."""
    value = ann["value"]
    ow = ann["original_width"]
    oh = ann["original_height"]
    return [
        value["x"] * ow / 100.0,
        value["y"] * oh / 100.0,
        value["width"] * ow / 100.0,
        value["height"] * oh / 100.0,
    ]


def fetch_tasks(conn, project_id: int) -> list[dict]:
    cur = conn.cursor()
    cur.execute(
        """
        SELECT t.id, t.data, tc.result
        FROM task t
        JOIN task_completion tc ON tc.task_id = t.id
        WHERE t.project_id = %s
        ORDER BY t.id
        """,
        (project_id,),
    )
    rows = []
    for task_id, data, result in cur.fetchall():
        rows.append({"task_id": task_id, "data": data, "result": result})
    return rows


def build_item(task: dict, data_dir: Path) -> dict | None:
    data = task["data"]
    result = task["result"]

    image_url = data.get("image", "")
    if not image_url:
        return None
    image_path = ls_url_to_path(image_url, data_dir)
    if not image_path.exists():
        print(f"  WARN image not found: {image_path}", file=sys.stderr)

    # Determine image dimensions from first annotation
    first = next((a for a in result if a.get("original_width")), None)
    if first is None:
        print(f"  WARN no width/height in task {task['task_id']}", file=sys.stderr)
        return None

    image_width = first["original_width"]
    image_height = first["original_height"]

    annotations = []
    for ann in result:
        if ann.get("type") != "rectanglelabels":
            continue
        value = ann.get("value", {})
        labels = value.get("rectanglelabels", [])
        if not labels:
            continue
        category = labels[0]
        bbox = convert_rect(ann)
        annotations.append({
            "id": ann.get("id", ""),
            "category": category,
            "bbox": bbox,
        })

    label_id = data.get("label_id", f"task_{task['task_id']}")
    # Store path relative to DATA_DIR (eval/datasets/) so manifest has no absolute paths.
    image_rel = image_url.replace("/data/local-files/?d=", "")
    return {
        "id": label_id,
        "image_rel_path": image_rel,
        "image_width": image_width,
        "image_height": image_height,
        "annotations": annotations,
    }


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    p.add_argument("--project-id", type=int, default=6, help="Label Studio project ID (default: 6)")
    p.add_argument("--pg-host", default="localhost")
    p.add_argument("--pg-port", type=int, default=5432)
    p.add_argument("--pg-user", default=os.getenv("POSTGRES_USER", "desktopctl"))
    p.add_argument("--pg-password", default=os.getenv("POSTGRES_PASSWORD", "desktopctl"))
    p.add_argument(
        "--data-dir",
        default=os.getenv("DATA_DIR"),
        help="Root of datasets dir (DATA_DIR env var)",
    )
    p.add_argument(
        "--output",
        default=str(MANIFEST_PATH),
        help="Output path for manifest.json",
    )
    return p.parse_args()


def main() -> None:
    args = parse_args()

    data_dir = Path(args.data_dir).resolve() if args.data_dir else DATA_DIR
    output_path = Path(args.output)

    conn = psycopg2.connect(
        host=args.pg_host,
        port=args.pg_port,
        dbname="labelstudio",
        user=args.pg_user,
        password=args.pg_password,
    )

    print(f"Fetching tasks from project #{args.project_id}...")
    tasks = fetch_tasks(conn, args.project_id)
    print(f"  found {len(tasks)} tasks with completions")

    items = []
    skipped = 0
    cat_counts: dict[str, int] = {}

    for task in tasks:
        item = build_item(task, data_dir)
        if item is None:
            skipped += 1
            continue
        items.append(item)
        for ann in item["annotations"]:
            cat = ann["category"]
            cat_counts[cat] = cat_counts.get(cat, 0) + 1

    manifest = {
        "version": 1,
        "generated_from": f"ls_project_{args.project_id}",
        "items": items,
    }

    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(json.dumps(manifest, indent=2))

    print(f"\n  exported: {len(items)}  skipped: {skipped}")
    print(f"  output:   {output_path}")
    print(f"\n  category counts:")
    for cat in sorted(cat_counts):
        print(f"    {cat}: {cat_counts[cat]}")

    # Verify all 6 expected categories are present
    expected_cats = {"text_field", "container", "text_or_paragraph", "button", "icon", "list"}
    missing = expected_cats - set(cat_counts.keys())
    if missing:
        print(f"\n  WARN missing categories: {missing}", file=sys.stderr)
    else:
        print(f"\n  OK: all 6 categories present")


if __name__ == "__main__":
    main()
