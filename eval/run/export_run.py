#!/usr/bin/env python3
"""Export accepted labels from Label Studio into a run folder.

Creates datasets/runs/<timestamp>-<slug>/artifacts.csv with one row per
accepted label. The run folder is then used as input for a tokenizer test run.

Usage:
    uv run run/export_run.py --project-id 3 --slug text_fields
    uv run run/export_run.py --project-id 3 --slug text_fields --control-type text_fields

Options:
    --project-id    LS project ID
    --slug          Short name for this run (e.g. text_fields, buttons)
    --control-type  Filter by control_type field (optional, exports all accepted if omitted)
    --verdict       Verdict to export (default: accept)

Environment (from eval/.env):
    POSTGRES_USER / POSTGRES_PASSWORD
    DATA_DIR  host path mounted as /data/local-files
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


def fetch_accepted(conn, project_id: int, verdict: str, control_type: str | None) -> list[dict]:
    cur = conn.cursor()
    cur.execute("""
        SELECT t.data
        FROM task t
        JOIN task_completion tc ON tc.task_id = t.id
        WHERE t.project_id = %s
          AND tc.result::text LIKE %s
    """, (project_id, f'%{verdict}%'))

    rows = []
    for (data,) in cur.fetchall():
        if control_type and data.get("control_type") != control_type:
            continue
        rows.append(data)
    return rows


def ls_url_to_path(url: str, data_dir: Path) -> Path:
    """Convert /data/local-files/?d=rel/path → absolute host path."""
    rel = url.replace("/data/local-files/?d=", "")
    return data_dir / rel


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--project-id", type=int, required=True)
    ap.add_argument("--slug", required=True, help="Short name for this run, e.g. text_fields")
    ap.add_argument("--control-type", default=None, help="Filter by control_type (optional)")
    ap.add_argument("--verdict", default="accept")
    ap.add_argument("--pg-host", default="localhost")
    ap.add_argument("--pg-port", type=int, default=5432)
    ap.add_argument("--pg-user", default=os.getenv("POSTGRES_USER", "desktopctl"))
    ap.add_argument("--pg-password", default=os.getenv("POSTGRES_PASSWORD"))
    ap.add_argument("--data-dir", default=os.getenv("DATA_DIR"))
    args = ap.parse_args()

    if not args.pg_password:
        sys.exit("POSTGRES_PASSWORD not set")
    if not args.data_dir:
        sys.exit("DATA_DIR not set")

    data_dir = Path(args.data_dir).resolve()
    run_id = datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S") + "-" + args.slug
    run_dir = Path(__file__).parent.parent / "datasets" / "runs" / run_id
    run_dir.mkdir(parents=True, exist_ok=True)

    conn = psycopg2.connect(
        host=args.pg_host, port=args.pg_port,
        dbname="labelstudio",
        user=args.pg_user, password=args.pg_password,
    )

    print(f"fetching {args.verdict} labels from project #{args.project_id}...")
    rows = fetch_accepted(conn, args.project_id, args.verdict, args.control_type)
    print(f"  found {len(rows)}")

    if not rows:
        sys.exit("nothing to export")

    csv_path = run_dir / "artifacts.csv"
    fieldnames = ["label_id", "sample_id", "control_type", "dataset",
                  "image_path", "overlay_path", "label_path", "metadata_path"]

    with csv_path.open("w", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=fieldnames)
        writer.writeheader()
        for data in rows:
            image_path = ls_url_to_path(data.get("image", ""), data_dir)
            overlay_path = ls_url_to_path(data.get("overlay_image", ""), data_dir)
            label_dir = image_path.parent
            writer.writerow({
                "label_id":      data.get("label_id", ""),
                "sample_id":     data.get("sample_id", ""),
                "control_type":  data.get("control_type", ""),
                "dataset":       data.get("dataset", ""),
                "image_path":    image_path,
                "overlay_path":  overlay_path,
                "label_path":    label_dir / "label.json",
                "metadata_path": label_dir / "metadata.json",
            })

    (run_dir / "meta.json").write_text(json.dumps({
        "run_id": run_id,
        "ls_project_id": args.project_id,
        "slug": args.slug,
        "control_type": args.control_type,
        "verdict": args.verdict,
        "n_artifacts": len(rows),
    }, indent=2))

    print(f"  run_id:  {run_id}")
    print(f"  written: {csv_path}")
    print(f"\nnext steps:")
    print(f"  just run-import {run_id}")
    print(f"  just run-log    {run_id}")


if __name__ == "__main__":
    main()
