#!/usr/bin/env python3
"""Build a mixed tokenize corpus from VM labels + borrowed MacPaw Screen2AX samples."""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any

DEFAULT_VM_LABEL_ROOT = Path(
    "/Users/oleg/Projects/DesktopCtl/tmp/tokenize-20260317-phase1/labels/selected/grounding_dino/broad_020_020_full52/grounding_dino"
)
DEFAULT_OUTPUT_ROOT = Path(
    "/Users/oleg/Projects/DesktopCtl/tmp/tokenize-20260317-phase1/labels/selected/mixed_vm_macpaw"
)
DEFAULT_GROUP_PARQUET_GLOB = "/Users/oleg/Projects/DesktopCtl/tmp/macpaw/Screen2AX-Group/data/*.parquet"
DEFAULT_ELEMENT_PARQUET_GLOB = "/Users/oleg/Projects/DesktopCtl/tmp/macpaw/Screen2AX-Element/data/*.parquet"

ELEMENT_ALLOWED = {
    "AXTextArea",
    "AXLink",
}
GROUP_ALLOWED = {
    "AXGroup",
    "AXScrollArea",
    "AXTable",
    "AXOutline",
    "AXList",
    "AXTabGroup",
    "AXSplitGroup",
    "AXToolbar",
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Assemble mixed tokenize corpus (VM labels + sampled Screen2AX labels)."
    )
    parser.add_argument(
        "--vm-label-root",
        type=Path,
        default=DEFAULT_VM_LABEL_ROOT,
        help="Root directory with existing VM *.labels.json files.",
    )
    parser.add_argument(
        "--output-root",
        type=Path,
        default=DEFAULT_OUTPUT_ROOT,
        help="Output corpus root.",
    )
    parser.add_argument(
        "--group-parquet-glob",
        default=DEFAULT_GROUP_PARQUET_GLOB,
        help="Glob for Screen2AX-Group parquet files.",
    )
    parser.add_argument(
        "--element-parquet-glob",
        default=DEFAULT_ELEMENT_PARQUET_GLOB,
        help="Glob for Screen2AX-Element parquet files.",
    )
    parser.add_argument(
        "--group-samples",
        type=int,
        default=30,
        help="How many group screenshots to borrow.",
    )
    parser.add_argument(
        "--element-samples",
        type=int,
        default=30,
        help="How many element screenshots to borrow.",
    )
    parser.add_argument(
        "--seed",
        default="20260319",
        help="Deterministic ordering seed.",
    )
    parser.add_argument(
        "--min-box-size",
        type=float,
        default=8.0,
        help="Reject boxes narrower/shorter than this.",
    )
    parser.add_argument(
        "--max-box-rel-area",
        type=float,
        default=0.72,
        help="Reject boxes larger than this fraction of image area.",
    )
    parser.add_argument(
        "--max-boxes-per-image",
        type=int,
        default=220,
        help="Cap per-image borrowed box labels.",
    )
    parser.add_argument(
        "--overwrite",
        action="store_true",
        help="Delete existing output root before writing.",
    )
    return parser.parse_args()


def sanitize_name(raw: str) -> str:
    chars = []
    for ch in raw:
        if ch.isalnum() or ch in ("-", "_"):
            chars.append(ch)
        else:
            chars.append("_")
    text = "".join(chars).strip("_")
    return text or "item"


def quote_sql_path(path: str) -> str:
    return "'" + path.replace("'", "''") + "'"


def run_duckdb_json(query: str) -> list[dict[str, Any]]:
    cmd = ["duckdb", "-json", "-c", query]
    proc = subprocess.run(cmd, capture_output=True, text=True)
    if proc.returncode != 0:
        raise RuntimeError(f"duckdb failed: {proc.stderr.strip()}")
    stdout = proc.stdout.strip()
    if not stdout:
        return []
    payload = json.loads(stdout)
    if isinstance(payload, list):
        return payload
    raise RuntimeError("unexpected duckdb JSON output format")


def discover_with_glob(glob_pattern: str) -> list[str]:
    from glob import glob

    files = sorted(glob(glob_pattern))
    return [str(Path(item).resolve()) for item in files if item.endswith(".parquet")]


def build_sample_query(parquet_files: list[str], limit: int, seed: str) -> str:
    if not parquet_files:
        raise ValueError("no parquet files matched")
    if limit <= 0:
        return "SELECT NULL WHERE FALSE"
    file_list = ", ".join(quote_sql_path(path) for path in parquet_files)
    seed_sql = seed.replace("'", "''")
    return f"""
SELECT
  image.path AS image_path,
  hex(image.bytes) AS image_hex,
  to_json(objects.bbox) AS bboxes_json,
  to_json(objects.category) AS categories_json
FROM read_parquet([{file_list}])
ORDER BY hash(coalesce(image.path, '') || '{seed_sql}')
LIMIT {int(limit)}
""".strip()


def bytes_ext(image_path: str, blob: bytes) -> str:
    suffix = Path(image_path).suffix.lower()
    if suffix in {".png", ".jpg", ".jpeg", ".webp"}:
        return suffix.lstrip(".")
    if blob.startswith(b"\x89PNG"):
        return "png"
    if blob.startswith(b"\xff\xd8\xff"):
        return "jpg"
    if blob[:4] == b"RIFF" and blob[8:12] == b"WEBP":
        return "webp"
    return "png"


def as_float_list(raw: Any) -> list[float]:
    if not isinstance(raw, list) or len(raw) < 4:
        return []
    out: list[float] = []
    for idx in range(4):
        try:
            out.append(float(raw[idx]))
        except (TypeError, ValueError):
            return []
    return out


def convert_boxes(
    bboxes: list[Any],
    categories: list[Any],
    allowed: set[str],
    *,
    min_box_size: float,
    max_box_rel_area: float,
    max_boxes_per_image: int,
) -> list[dict[str, Any]]:
    if not bboxes:
        return []
    max_pairs = min(len(bboxes), len(categories))
    filtered: list[tuple[float, dict[str, Any]]] = []

    max_x = 0.0
    max_y = 0.0
    parsed: list[tuple[str, float, float, float, float]] = []
    for idx in range(max_pairs):
        category = str(categories[idx])
        if category not in allowed:
            continue
        row = as_float_list(bboxes[idx])
        if not row:
            continue
        x1, y1, x2, y2 = row
        x = min(x1, x2)
        y = min(y1, y2)
        w = abs(x2 - x1)
        h = abs(y2 - y1)
        if w < min_box_size or h < min_box_size:
            continue
        max_x = max(max_x, x + w)
        max_y = max(max_y, y + h)
        parsed.append((category, x, y, w, h))

    if not parsed:
        return []

    image_area_est = max(1.0, max_x * max_y)
    for category, x, y, w, h in parsed:
        rel_area = (w * h) / image_area_est
        if rel_area > max_box_rel_area:
            continue
        aspect = max(w / max(h, 1.0), h / max(w, 1.0))
        if aspect > 22.0:
            continue
        element = {
            "type": "box",
            "bbox": [round(x, 1), round(y, 1), round(w, 1), round(h, 1)],
            "source": f"macpaw:{category}",
        }
        score = rel_area
        filtered.append((score, element))

    filtered.sort(key=lambda item: item[0])
    trimmed = [item[1] for item in filtered[:max_boxes_per_image]]
    for idx, element in enumerate(trimmed, start=1):
        element["id"] = f"box_{idx:04d}"
    return trimmed


def copy_vm_labels(vm_label_root: Path, output_root: Path) -> int:
    src_files = sorted(path for path in vm_label_root.rglob("*.labels.json") if path.is_file())
    out_root = output_root / "vm"
    copied = 0
    for src in src_files:
        dest = out_root / src.relative_to(vm_label_root)
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(src, dest)
        copied += 1
    return copied


def borrow_dataset(
    *,
    dataset_name: str,
    parquet_glob: str,
    samples: int,
    seed: str,
    output_root: Path,
    allowed_categories: set[str],
    min_box_size: float,
    max_box_rel_area: float,
    max_boxes_per_image: int,
) -> tuple[int, int]:
    parquet_files = discover_with_glob(parquet_glob)
    if not parquet_files or samples <= 0:
        return 0, 0

    query = build_sample_query(parquet_files, samples, seed)
    rows = run_duckdb_json(query)

    out_dir = output_root / "macpaw" / dataset_name
    out_dir.mkdir(parents=True, exist_ok=True)

    emitted = 0
    skipped = 0
    for idx, row in enumerate(rows, start=1):
        image_path = str(row.get("image_path") or f"{dataset_name}_{idx:04d}")
        image_hex = row.get("image_hex")
        bboxes_json = row.get("bboxes_json")
        categories_json = row.get("categories_json")
        try:
            if isinstance(bboxes_json, list):
                bboxes = bboxes_json
            elif isinstance(bboxes_json, str):
                bboxes = json.loads(bboxes_json)
            else:
                bboxes = []
            if isinstance(categories_json, list):
                categories = categories_json
            elif isinstance(categories_json, str):
                categories = json.loads(categories_json)
            else:
                categories = []
        except json.JSONDecodeError:
            skipped += 1
            continue

        if not image_hex or not isinstance(image_hex, str):
            skipped += 1
            continue

        try:
            blob = bytes.fromhex(image_hex)
        except ValueError:
            skipped += 1
            continue

        elements = convert_boxes(
            bboxes,
            categories,
            allowed_categories,
            min_box_size=min_box_size,
            max_box_rel_area=max_box_rel_area,
            max_boxes_per_image=max_boxes_per_image,
        )
        if not elements:
            skipped += 1
            continue

        ext = bytes_ext(image_path, blob)
        stem = sanitize_name(Path(image_path).stem)[:48]
        digest = hashlib.sha1(blob).hexdigest()[:10]
        base = f"{dataset_name}_{idx:04d}_{stem}_{digest}"
        image_out = (out_dir / f"{base}.{ext}").resolve()
        label_out = out_dir / f"{base}.labels.json"

        image_out.write_bytes(blob)
        if ext != "png":
            png_out = image_out.with_suffix(".png")
            proc = subprocess.run(
                ["sips", "-s", "format", "png", str(image_out), "--out", str(png_out)],
                capture_output=True,
                text=True,
            )
            if proc.returncode != 0:
                skipped += 1
                image_out.unlink(missing_ok=True)
                continue
            image_out.unlink(missing_ok=True)
            image_out = png_out.resolve()
        payload = {
            "image": {
                "path": str(image_out),
            },
            "windows": [
                {
                    "id": "window_0001",
                    "elements": elements,
                }
            ],
            "meta": {
                "borrowed_from": f"Screen2AX-{dataset_name.title()}",
                "seed": seed,
                "parquet_glob": parquet_glob,
            },
        }
        label_out.write_text(json.dumps(payload, indent=2), encoding="utf-8")
        emitted += 1

    return emitted, skipped


def main() -> int:
    args = parse_args()

    vm_label_root = args.vm_label_root.expanduser().resolve()
    output_root = args.output_root.expanduser().resolve()
    if args.overwrite and output_root.exists():
        shutil.rmtree(output_root)
    output_root.mkdir(parents=True, exist_ok=True)

    if not vm_label_root.exists():
        raise SystemExit(f"VM label root does not exist: {vm_label_root}")

    vm_count = copy_vm_labels(vm_label_root, output_root)

    group_emitted, group_skipped = borrow_dataset(
        dataset_name="group",
        parquet_glob=args.group_parquet_glob,
        samples=args.group_samples,
        seed=f"{args.seed}:group",
        output_root=output_root,
        allowed_categories=GROUP_ALLOWED,
        min_box_size=args.min_box_size,
        max_box_rel_area=args.max_box_rel_area,
        max_boxes_per_image=args.max_boxes_per_image,
    )
    element_emitted, element_skipped = borrow_dataset(
        dataset_name="element",
        parquet_glob=args.element_parquet_glob,
        samples=args.element_samples,
        seed=f"{args.seed}:element",
        output_root=output_root,
        allowed_categories=ELEMENT_ALLOWED,
        min_box_size=args.min_box_size,
        max_box_rel_area=args.max_box_rel_area,
        max_boxes_per_image=args.max_boxes_per_image,
    )

    total_labels = len(list(output_root.rglob("*.labels.json")))
    summary = {
        "output_root": str(output_root),
        "vm_labels_copied": vm_count,
        "macpaw_group_emitted": group_emitted,
        "macpaw_group_skipped": group_skipped,
        "macpaw_element_emitted": element_emitted,
        "macpaw_element_skipped": element_skipped,
        "total_labels": total_labels,
        "seed": args.seed,
    }
    (output_root / "_summary.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
