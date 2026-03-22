#!/usr/bin/env python3
"""Add raw image(s) to a golden label dataset directory and import into Label Studio.

Creates one label subdir per image containing image.png, overlay.png (copy of image),
metadata.json, and label.json, then imports into the given LS project.

Usage:
    uv run import/golden_add.py \\
        --images path/to/img1.png path/to/img2.png \\
        --dest datasets/labels/buttons \\
        --project-id 6 \\
        --control-type buttons

    # dry-run: prepare dirs but skip LS import
    uv run import/golden_add.py --images ... --dest ... --project-id 6 --dry-run
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import struct
import sys
from pathlib import Path

import requests
from dotenv import load_dotenv

load_dotenv(Path(__file__).parent.parent / ".env")


def png_size(path: Path) -> tuple[int, int]:
    with open(path, "rb") as fh:
        fh.read(8)   # signature
        fh.read(4)   # length
        fh.read(4)   # IHDR
        w = struct.unpack(">I", fh.read(4))[0]
        h = struct.unpack(">I", fh.read(4))[0]
    return w, h


def file_hash(path: Path, n: int = 10) -> str:
    return hashlib.md5(path.read_bytes()).hexdigest()[:n]


def make_label_id(src: Path, control_type: str, index: int) -> str:
    stem = src.stem  # e.g. file_open_light
    h = file_hash(src)
    return f"{stem}_{index:04d}_{control_type}_{h}"


def prepare_label_dir(src: Path, dest_root: Path, control_type: str, index: int, dataset: str) -> Path:
    label_id = make_label_id(src, control_type, index)
    label_dir = dest_root / label_id
    label_dir.mkdir(parents=True, exist_ok=True)

    shutil.copy2(src, label_dir / "image.png")
    shutil.copy2(src, label_dir / "overlay.png")

    w, h = png_size(src)

    metadata = {
        "label_id": label_id,
        "sample_id": f"{src.stem}_{index:04d}",
        "dataset": dataset,
        "control_type": control_type,
        "source": "manual",
        "image_width": w,
        "image_height": h,
        "image_path": "image.png",
        "overlay_path": "overlay.png",
    }
    (label_dir / "metadata.json").write_text(json.dumps(metadata, indent=2))
    (label_dir / "label.json").write_text("{}")

    return label_dir


def ls_headers(api_key: str) -> dict[str, str]:
    return {"Authorization": f"Token {api_key}", "Content-Type": "application/json"}


def path_to_ls_url(path: Path, data_dir: Path) -> str:
    rel = path.relative_to(data_dir)
    return f"/data/local-files/?d={rel}"


def build_task(label_dir: Path, data_dir: Path) -> dict:
    metadata = json.loads((label_dir / "metadata.json").read_text())
    overlay_path = label_dir / "overlay.png"
    image_path = label_dir / "image.png"
    return {
        "data": {
            "overlay_image": path_to_ls_url(overlay_path, data_dir),
            "image": path_to_ls_url(image_path, data_dir),
            "label_id": metadata["label_id"],
            "sample_id": metadata.get("sample_id", ""),
            "control_type": metadata.get("control_type", ""),
            "dataset": metadata.get("dataset", ""),
            "metadata": metadata,
        }
    }


def import_tasks(base_url: str, api_key: str, project_id: int, tasks: list[dict]) -> None:
    hdrs = ls_headers(api_key)
    r = requests.post(
        f"{base_url}/api/projects/{project_id}/import",
        headers=hdrs,
        json=tasks,
    )
    r.raise_for_status()
    print(f"  imported {r.json().get('task_count', len(tasks))} tasks into project #{project_id}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--images", nargs="+", required=True, help="Source image file(s) to add")
    ap.add_argument("--dest", required=True, help="Label dataset directory (e.g. datasets/labels/buttons)")
    ap.add_argument("--project-id", type=int, required=True, help="Label Studio project ID")
    ap.add_argument("--control-type", default="buttons", help="Control type tag (default: buttons)")
    ap.add_argument("--dataset", default="golden_manual", help="Dataset tag (default: golden_manual)")
    ap.add_argument("--ls-url", default=os.getenv("LS_URL", "http://localhost:8080"))
    ap.add_argument("--ls-key", default=os.getenv("LS_API_KEY"))
    ap.add_argument("--data-dir", default=os.getenv("DATA_DIR"))
    ap.add_argument("--dry-run", action="store_true", help="Prepare dirs but skip LS import")
    args = ap.parse_args()

    if not args.dry_run:
        if not args.ls_key:
            sys.exit("LS_API_KEY not set")
        if not args.data_dir:
            sys.exit("DATA_DIR not set")

    dest_root = Path(args.dest).resolve()
    data_dir = Path(args.data_dir).resolve() if args.data_dir else None

    label_dirs: list[Path] = []
    for i, img_path in enumerate(args.images, start=1):
        src = Path(img_path).resolve()
        if not src.exists():
            sys.exit(f"image not found: {src}")
        label_dir = prepare_label_dir(src, dest_root, args.control_type, i, args.dataset)
        print(f"  prepared {label_dir.name}")
        label_dirs.append(label_dir)

    if args.dry_run:
        print("dry-run — skipping LS import")
        return

    tasks = [build_task(d, data_dir) for d in label_dirs]  # type: ignore[arg-type]
    import_tasks(args.ls_url, args.ls_key, args.project_id, tasks)
    print(f"done → {args.ls_url}/projects/{args.project_id}/")


if __name__ == "__main__":
    main()
