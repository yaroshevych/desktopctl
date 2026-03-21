#!/usr/bin/env python3
"""Read Label Studio annotations and log metrics to MLflow.

The run_id is used as both the LS project name and the MLflow run name.

Usage:
    uv run run/log_run.py 20260321-143000-text_fields
    uv run run/log_run.py 20260321-143000-text_fields --dry-run

Environment (from eval/.env):
    POSTGRES_USER / POSTGRES_PASSWORD
    MLFLOW_TRACKING_URI  (default: http://localhost:5001)
"""

from __future__ import annotations

import argparse
import csv
import json
import os
import subprocess
import sys
from pathlib import Path

import mlflow
import psycopg2
from dotenv import load_dotenv

load_dotenv(Path(__file__).parent.parent / ".env")


def git_hash() -> str:
    try:
        return subprocess.check_output(
            ["git", "rev-parse", "HEAD"],
            cwd=Path(__file__).parent.parent.parent,
        ).strip().decode()
    except Exception:
        return "unknown"


def get_project_id(conn, project_name: str) -> int:
    cur = conn.cursor()
    cur.execute("SELECT id FROM project WHERE title = %s", (project_name,))
    row = cur.fetchone()
    if not row:
        sys.exit(f"no LS project named '{project_name}' — import results first")
    return row[0]


def fetch_annotations(conn, project_id: int) -> list[dict]:
    cur = conn.cursor()
    cur.execute("""
        SELECT t.data, tc.result
        FROM task t
        JOIN task_completion tc ON tc.task_id = t.id
        WHERE t.project_id = %s
        ORDER BY t.id
    """, (project_id,))
    return [{"task_data": data, "result": result} for data, result in cur.fetchall()]


def extract_annotation(row: dict) -> dict:
    result = row["result"]
    if isinstance(result, str):
        result = json.loads(result)
    verdict = None
    comment = ""
    for item in result or []:
        if item.get("from_name") == "verdict":
            vals = item.get("value", {}).get("choices", [])
            verdict = vals[0] if vals else None
        elif item.get("from_name") == "comments":
            texts = item.get("value", {}).get("text", [])
            comment = texts[0] if texts else ""
    td = row["task_data"]
    return {
        "label_id":     td.get("label_id", ""),
        "sample_id":    td.get("sample_id", ""),
        "control_type": td.get("control_type", ""),
        "dataset":      td.get("dataset", ""),
        "verdict":      verdict or "",
        "comment":      comment,
    }


def write_csv(rows: list[dict], path: Path) -> None:
    fieldnames = ["label_id", "sample_id", "control_type", "dataset", "verdict", "comment"]
    with path.open("w", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=fieldnames)
        writer.writeheader()
        writer.writerows(rows)


def compute_metrics(rows: list[dict]) -> dict[str, float]:
    counts: dict[str, int] = {}
    for row in rows:
        result = row["result"]
        if isinstance(result, str):
            result = json.loads(result)
        verdict = None
        for item in result or []:
            if item.get("from_name") == "verdict":
                vals = item.get("value", {}).get("choices", [])
                verdict = vals[0] if vals else None
        counts[verdict or "unlabeled"] = counts.get(verdict or "unlabeled", 0) + 1

    total = len(rows)
    metrics: dict[str, float] = {"n_total": float(total)}
    for label, n in counts.items():
        metrics[f"n_{label}"] = float(n)
        if total:
            metrics[f"pct_{label}"] = n / total
    return metrics


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("run_id", help="Run ID — must match the LS project name")
    ap.add_argument("--experiment", default="tokenizer")
    ap.add_argument("--pg-host", default="localhost")
    ap.add_argument("--pg-port", type=int, default=5432)
    ap.add_argument("--pg-user", default=os.getenv("POSTGRES_USER", "desktopctl"))
    ap.add_argument("--pg-password", default=os.getenv("POSTGRES_PASSWORD"))
    ap.add_argument("--mlflow-uri", default=os.getenv("MLFLOW_TRACKING_URI", "http://localhost:5001"))
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    if not args.pg_password:
        sys.exit("POSTGRES_PASSWORD not set")

    conn = psycopg2.connect(
        host=args.pg_host, port=args.pg_port,
        dbname="labelstudio",
        user=args.pg_user, password=args.pg_password,
    )

    project_id = get_project_id(conn, args.run_id)
    print(f"project: #{project_id} '{args.run_id}'")

    rows = fetch_annotations(conn, project_id)
    print(f"annotations: {len(rows)}")
    if not rows:
        sys.exit("no annotations — label some tasks first")

    metrics = compute_metrics(rows)
    ghash = git_hash()

    print(f"git:     {ghash[:8]}")
    print("metrics:")
    for k, v in sorted(metrics.items()):
        print(f"  {k:20s} {v:.3f}" if isinstance(v, float) else f"  {k:20s} {v}")

    if args.dry_run:
        print("\ndry-run — nothing logged")
        return

    mlflow.set_tracking_uri(args.mlflow_uri)
    mlflow.set_experiment(args.experiment)

    run_dir = Path(__file__).parent.parent / "datasets" / "runs" / args.run_id
    csv_path = run_dir / "annotations.csv"
    annotations = [extract_annotation(r) for r in rows]
    write_csv(annotations, csv_path)
    print(f"annotations csv: {csv_path}")

    with mlflow.start_run(run_name=args.run_id):
        mlflow.set_tag("git_hash", ghash)
        mlflow.set_tag("git_hash_short", ghash[:8])
        mlflow.set_tag("ls_project_id", str(project_id))
        mlflow.log_metrics(metrics)
        mlflow.log_artifact(str(csv_path))

    print(f"\nlogged → {args.mlflow_uri}/#/experiments")


if __name__ == "__main__":
    main()
