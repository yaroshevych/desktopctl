#!/usr/bin/env python3
"""Run desktopctl tokenizer for a run folder and emit Label Studio-ready results."""

from __future__ import annotations

import argparse
import concurrent.futures
import csv
import hashlib
import json
import os
import shutil
import subprocess
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--run-dir",
        type=Path,
        required=True,
        help="Run directory containing artifacts.csv (eval/datasets/runs/<run_id>).",
    )
    parser.add_argument(
        "--desktopctl-bin",
        type=Path,
        default=Path("/Users/oleg/Projects/DesktopCtl/src/desktop/dist/desktopctl"),
        help="Path to desktopctl binary.",
    )
    parser.add_argument(
        "--daemon-bin",
        type=Path,
        default=Path(
            "/Users/oleg/Projects/DesktopCtl/src/desktop/dist/DesktopCtl.app/Contents/MacOS/desktopctld"
        ),
        help="Path to desktopctld binary.",
    )
    parser.add_argument(
        "--tokenize-dump-bin",
        type=Path,
        default=Path("/Users/oleg/Projects/DesktopCtl/src/desktop/target/release/tokenize_dump"),
        help="Path to tokenize_dump binary (fast batch engine).",
    )
    parser.add_argument(
        "--engine",
        choices=["dump", "desktopctl"],
        default="dump",
        help="Tokenizer engine: dump (fast) or desktopctl (daemon path).",
    )
    parser.add_argument(
        "--results-subdir",
        default="results",
        help="Subdirectory under run-dir to write outputs.",
    )
    parser.add_argument(
        "--limit",
        type=int,
        default=0,
        help="Optional max number of rows from artifacts.csv (0 = all).",
    )
    parser.add_argument(
        "--overwrite",
        action="store_true",
        help="Delete existing results subdir before running.",
    )
    parser.add_argument(
        "--copy-image",
        action="store_true",
        default=False,
        help="Copy source image to results/<label_id>/image.png.",
    )
    parser.add_argument(
        "--jobs",
        type=int,
        default=0,
        help="Parallel workers for tokenize_dump. 0 = auto (desktopctl forces 1).",
    )
    parser.add_argument(
        "--skip-existing",
        action="store_true",
        help="Skip rows that already have overlay.png, label.json and metadata.json.",
    )
    parser.add_argument(
        "--no-dedupe",
        action="store_true",
        help="Disable source-image dedupe (default dedupes identical image paths).",
    )
    parser.add_argument(
        "--use-isolated-cli",
        action="store_true",
        default=True,
        help="Copy desktopctl into run-local .bin dir to avoid app-bundle auto-open path.",
    )
    parser.add_argument(
        "--no-isolated-cli",
        action="store_true",
        help="Use desktopctl binary directly (no copy).",
    )
    return parser.parse_args()


def read_artifacts(path: Path) -> list[dict[str, str]]:
    with path.open(newline="", encoding="utf-8") as fh:
        reader = csv.DictReader(fh)
        rows = [dict(row) for row in reader]
    return rows


def to_label_payload(stdout: str) -> dict[str, Any]:
    text = stdout.strip()
    if not text:
        return {"raw_stdout": "", "parse_error": "empty_stdout"}
    try:
        return json.loads(text)
    except json.JSONDecodeError as exc:
        return {
            "raw_stdout": text,
            "parse_error": f"{type(exc).__name__}: {exc}",
        }


def copy_image_png(src: Path, dst_png: Path) -> None:
    if src.suffix.lower() == ".png":
        shutil.copy2(src, dst_png)
        return
    # Prefer macOS sips to preserve visual fidelity for JPG/other formats.
    proc = subprocess.run(
        ["sips", "-s", "format", "png", str(src), "--out", str(dst_png)],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"sips convert failed: {proc.stderr.strip() or proc.stdout.strip()}")


def maybe_prepare_isolated_cli(
    *,
    run_dir: Path,
    desktopctl_bin: Path,
    enabled: bool,
) -> Path:
    if not enabled:
        return desktopctl_bin
    isolated_dir = run_dir / ".bin"
    isolated_dir.mkdir(parents=True, exist_ok=True)
    isolated_cli = isolated_dir / "desktopctl"
    if not isolated_cli.exists() or isolated_cli.stat().st_mtime_ns != desktopctl_bin.stat().st_mtime_ns:
        shutil.copy2(desktopctl_bin, isolated_cli)
        isolated_cli.chmod(0o755)
    return isolated_cli


def required_outputs_exist(out_dir: Path) -> bool:
    return all((out_dir / name).exists() for name in ("overlay.png", "label.json", "metadata.json"))


def now_utc() -> str:
    return datetime.now(timezone.utc).isoformat()


def run_tokenize_once(
    *,
    engine: str,
    run_dir: Path,
    source_image: Path,
    label_out: Path,
    overlay_out: Path,
    cli_bin: Path | None,
    daemon_bin: Path,
    tokenize_dump_bin: Path,
) -> dict[str, Any]:
    env = dict(os.environ)
    if engine == "desktopctl":
        env["DESKTOPCTL_DAEMON_BIN"] = str(daemon_bin)
        env["DESKTOPCTL_AUTOSTART_MODE"] = "on-demand"
        command = [
            str(cli_bin),
            "screen",
            "tokenize",
            "--json",
            "--overlay",
            str(overlay_out),
            "--screenshot",
            str(source_image),
        ]
    else:
        command = [
            str(tokenize_dump_bin),
            "--input",
            str(source_image),
            "--json",
            str(label_out),
            "--overlay",
            str(overlay_out),
        ]
    proc = subprocess.run(
        command,
        cwd=str(run_dir),
        capture_output=True,
        text=True,
        check=False,
        env=env,
    )
    return {
        "ok": proc.returncode == 0,
        "exit_code": proc.returncode,
        "stdout": proc.stdout,
        "stderr": proc.stderr,
        "command": command,
    }


def write_failure_payload(
    *,
    label_out: Path,
    error: str,
    exit_code: int | None,
    stdout: str,
    stderr: str,
    command: list[str] | None,
) -> None:
    label_out.write_text(
        json.dumps(
            {
                "error": error,
                "exit_code": exit_code,
                "stdout": stdout,
                "stderr": stderr,
                "command": command,
            },
            indent=2,
        ),
        encoding="utf-8",
    )


def main() -> int:
    args = parse_args()
    run_dir = args.run_dir.expanduser().resolve()
    artifacts_csv = run_dir / "artifacts.csv"
    if not run_dir.exists():
        raise SystemExit(f"run dir not found: {run_dir}")
    if not artifacts_csv.exists():
        raise SystemExit(f"missing artifacts.csv: {artifacts_csv}")

    desktopctl_bin = args.desktopctl_bin.expanduser().resolve()
    daemon_bin = args.daemon_bin.expanduser().resolve()
    tokenize_dump_bin = args.tokenize_dump_bin.expanduser().resolve()
    if args.engine == "desktopctl":
        if not desktopctl_bin.exists():
            raise SystemExit(f"desktopctl binary not found: {desktopctl_bin}")
        if not daemon_bin.exists():
            raise SystemExit(f"daemon binary not found: {daemon_bin}")
    if args.engine == "dump" and not tokenize_dump_bin.exists():
        raise SystemExit(f"tokenize_dump binary not found: {tokenize_dump_bin}")

    results_dir = run_dir / args.results_subdir
    if args.overwrite and results_dir.exists():
        shutil.rmtree(results_dir)
    results_dir.mkdir(parents=True, exist_ok=True)

    copy_image = bool(args.copy_image)
    isolate_cli = bool(args.use_isolated_cli and not args.no_isolated_cli)
    cli_bin = None
    if args.engine == "desktopctl":
        cli_bin = maybe_prepare_isolated_cli(
            run_dir=run_dir,
            desktopctl_bin=desktopctl_bin,
            enabled=isolate_cli,
        )

    rows = read_artifacts(artifacts_csv)
    if args.limit > 0:
        rows = rows[: args.limit]
    if not rows:
        raise SystemExit("no rows in artifacts.csv after applying limit")

    generated = 0
    failed = 0
    skipped_existing = 0
    dedupe_reused = 0
    failures: list[dict[str, str]] = []
    pending_rows: list[dict[str, Any]] = []

    for idx, row in enumerate(rows, start=1):
        label_id = (row.get("label_id") or "").strip()
        image_path_str = (row.get("image_path") or "").strip()
        if not label_id or not image_path_str:
            failed += 1
            failures.append(
                {
                    "label_id": label_id or f"row_{idx:04d}",
                    "error": "missing label_id or image_path in artifacts.csv",
                }
            )
            continue

        source_image = Path(image_path_str).expanduser()
        if not source_image.exists():
            failed += 1
            failures.append(
                {
                    "label_id": label_id,
                    "error": f"source image missing: {source_image}",
                }
            )
            continue

        out_dir = results_dir / label_id
        out_dir.mkdir(parents=True, exist_ok=True)
        if args.skip_existing and required_outputs_exist(out_dir):
            skipped_existing += 1
            continue

        pending_rows.append(
            {
                "row": row,
                "label_id": label_id,
                "source_image": source_image.resolve(),
                "out_dir": out_dir,
                "overlay_out": out_dir / "overlay.png",
                "label_out": out_dir / "label.json",
                "metadata_out": out_dir / "metadata.json",
                "image_out": out_dir / "image.png",
            }
        )

    dedupe_enabled = not args.no_dedupe
    groups: dict[str, list[dict[str, Any]]] = {}
    for task in pending_rows:
        group_key = (
            str(task["source_image"])
            if dedupe_enabled
            else f"{task['label_id']}::{task['source_image']}"
        )
        groups.setdefault(group_key, []).append(task)

    group_values = list(groups.values())
    default_jobs = max(1, min(8, os.cpu_count() or 4))
    jobs = 1 if args.engine == "desktopctl" else (args.jobs if args.jobs > 0 else default_jobs)
    jobs = max(1, jobs)

    def process_group(group: list[dict[str, Any]]) -> tuple[list[dict[str, Any]], dict[str, Any]]:
        canonical = group[0]
        result = run_tokenize_once(
            engine=args.engine,
            run_dir=run_dir,
            source_image=canonical["source_image"],
            label_out=canonical["label_out"],
            overlay_out=canonical["overlay_out"],
            cli_bin=cli_bin,
            daemon_bin=daemon_bin,
            tokenize_dump_bin=tokenize_dump_bin,
        )
        return group, result

    group_results: list[tuple[list[dict[str, Any]], dict[str, Any]]] = []
    if jobs == 1 or len(group_values) <= 1:
        for group in group_values:
            group_results.append(process_group(group))
    else:
        with concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as pool:
            futures = [pool.submit(process_group, group) for group in group_values]
            for future in concurrent.futures.as_completed(futures):
                group_results.append(future.result())

    for group, result in group_results:
        canonical = group[0]
        command = result.get("command")
        stdout = result.get("stdout", "")
        stderr = result.get("stderr", "")
        exit_code = result.get("exit_code")

        if result.get("ok"):
            if args.engine == "desktopctl":
                payload = to_label_payload(stdout)
                canonical["label_out"].write_text(
                    json.dumps(payload, indent=2),
                    encoding="utf-8",
                )
            elif not canonical["label_out"].exists():
                write_failure_payload(
                    label_out=canonical["label_out"],
                    error="missing_label_from_dump",
                    exit_code=None,
                    stdout=stdout,
                    stderr=stderr,
                    command=command,
                )
                result = {
                    "ok": False,
                    "exit_code": None,
                    "stdout": stdout,
                    "stderr": f"label missing after tokenize: {canonical['label_out']}",
                    "command": command,
                }

            if not canonical["overlay_out"].exists():
                result = {
                    "ok": False,
                    "exit_code": None,
                    "stdout": stdout,
                    "stderr": f"overlay missing after tokenize: {canonical['overlay_out']}",
                    "command": command,
                }

        if not result.get("ok"):
            for task in group:
                failed += 1
                failures.append(
                    {
                        "label_id": task["label_id"],
                        "error": f"tokenize_failed exit={result.get('exit_code')}",
                    }
                )
                write_failure_payload(
                    label_out=task["label_out"],
                    error="tokenize_failed",
                    exit_code=result.get("exit_code"),
                    stdout=result.get("stdout", ""),
                    stderr=result.get("stderr", ""),
                    command=result.get("command"),
                )
                task["metadata_out"].write_text(
                    json.dumps(
                        {
                            "run_id": run_dir.name,
                            "label_id": task["label_id"],
                            "sample_id": task["row"].get("sample_id", ""),
                            "control_type": task["row"].get("control_type", ""),
                            "dataset": task["row"].get("dataset", ""),
                            "source_image_path": str(task["source_image"]),
                            "status": "failed",
                            "generated_at_utc": now_utc(),
                        },
                        indent=2,
                    ),
                    encoding="utf-8",
                )
            continue

        for task in group[1:]:
            shutil.copy2(canonical["overlay_out"], task["overlay_out"])
            shutil.copy2(canonical["label_out"], task["label_out"])
            dedupe_reused += 1

        for task in group:
            if copy_image:
                try:
                    copy_image_png(task["source_image"], task["image_out"])
                except Exception as exc:  # pragma: no cover
                    failed += 1
                    failures.append({"label_id": task["label_id"], "error": f"copy_image_failed: {exc}"})
                    write_failure_payload(
                        label_out=task["label_out"],
                        error="copy_image_failed",
                        exit_code=None,
                        stdout=stdout,
                        stderr=str(exc),
                        command=command,
                    )
                    task["metadata_out"].write_text(
                        json.dumps(
                            {
                                "run_id": run_dir.name,
                                "label_id": task["label_id"],
                                "sample_id": task["row"].get("sample_id", ""),
                                "control_type": task["row"].get("control_type", ""),
                                "dataset": task["row"].get("dataset", ""),
                                "source_image_path": str(task["source_image"]),
                                "status": "failed",
                                "generated_at_utc": now_utc(),
                            },
                            indent=2,
                        ),
                        encoding="utf-8",
                    )
                    continue

            metadata = {
                "run_id": run_dir.name,
                "label_id": task["label_id"],
                "sample_id": task["row"].get("sample_id", ""),
                "control_type": task["row"].get("control_type", ""),
                "dataset": task["row"].get("dataset", ""),
                "source_image_path": str(task["source_image"]),
                "source_overlay_path": task["row"].get("overlay_path", ""),
                "source_label_path": task["row"].get("label_path", ""),
                "source_metadata_path": task["row"].get("metadata_path", ""),
                "engine": args.engine,
                "tokenizer_cli": str(cli_bin) if cli_bin else None,
                "tokenize_dump_bin": str(tokenize_dump_bin),
                "daemon_bin": str(daemon_bin) if args.engine == "desktopctl" else None,
                "command": " ".join(command or []),
                "status": "ok",
                "stdout_sha1": hashlib.sha1(stdout.encode("utf-8")).hexdigest(),
                "generated_at_utc": now_utc(),
                "image_path": "image.png" if copy_image else None,
                "overlay_path": "overlay.png",
                "label_path": "label.json",
                "dedupe_source_label_id": canonical["label_id"],
                "dedupe_group_size": len(group),
            }
            task["metadata_out"].write_text(json.dumps(metadata, indent=2), encoding="utf-8")
            generated += 1

    summary = {
        "run_id": run_dir.name,
        "artifacts_csv": str(artifacts_csv),
        "results_dir": str(results_dir),
        "rows_total": len(rows),
        "generated": generated,
        "failed": failed,
        "skipped_existing": skipped_existing,
        "copy_image": copy_image,
        "engine": args.engine,
        "isolate_cli": isolate_cli,
        "jobs": jobs,
        "dedupe_enabled": dedupe_enabled,
        "dedupe_reused": dedupe_reused,
        "pending_rows": len(pending_rows),
        "unique_images_processed": len(group_values),
        "desktopctl_bin": str(desktopctl_bin),
        "tokenize_dump_bin": str(tokenize_dump_bin),
        "daemon_bin": str(daemon_bin),
        "failures": failures[:200],
        "generated_at_utc": datetime.now(timezone.utc).isoformat(),
    }
    summary_path = run_dir / "_tokenize_summary.json"
    summary_path.write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(json.dumps(summary, indent=2))
    return 0 if failed == 0 else 2


if __name__ == "__main__":
    raise SystemExit(main())
