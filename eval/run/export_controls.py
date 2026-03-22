#!/usr/bin/env python3
"""Export text_field and button annotations from Label Studio into a JSON
fixture for the Rust `text_field_button_labels` integration test.

Connects to the LS postgres DB, fetches tasks from the golden-set project,
filters to text_field/button annotations, runs macOS Vision OCR to fill in
text labels, and writes:
  src/desktop/daemon/tests/fixtures/golden/controls.json

Usage:
    uv run run/export_controls.py
    uv run run/export_controls.py --project-id 6
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from pathlib import Path

import psycopg2
from dotenv import load_dotenv

load_dotenv(Path(__file__).parent.parent / ".env")

REPO_ROOT = Path(__file__).parent.parent.parent
DATA_DIR = Path(os.getenv("DATA_DIR", str(Path(__file__).parent.parent / "datasets")))
FIXTURE_DIR = REPO_ROOT / "src/desktop/daemon/tests/fixtures/golden"
OUTPUT_PATH = FIXTURE_DIR / "controls.json"

GOLDEN_FIXTURE_FILES = {p.name for p in FIXTURE_DIR.glob("*.png")}

CONTROL_CATEGORIES = {"text_field", "button"}


# ── LS helpers (shared with export_golden_manifest.py) ───────────────────────

def ls_url_to_path(url: str, data_dir: Path) -> Path:
    rel = url.replace("/data/local-files/?d=", "")
    return data_dir / rel


def convert_rect(ann: dict) -> list[float]:
    """Convert LS percent-based rect annotation to pixel [x, y, w, h]."""
    value = ann["value"]
    ow = ann["original_width"]
    oh = ann["original_height"]
    return [
        round(value["x"] * ow / 100.0, 1),
        round(value["y"] * oh / 100.0, 1),
        round(value["width"] * ow / 100.0, 1),
        round(value["height"] * oh / 100.0, 1),
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


# ── fixture filename mapping ─────────────────────────────────────────────────

def item_id_to_fixture_filename(item_id: str) -> str | None:
    """Extract fixture filename from LS item ID.

    Example: 'dictionary__dictionary_default_light_0022_text_fields_901f59a0e6'
    → 'dictionary_default_light.png'
    """
    parts = item_id.split("__")
    if len(parts) < 2:
        return None
    suffix = parts[1]
    m = re.match(r"^(\w+_default_(?:dark|light))_\d+", suffix)
    if not m:
        return None
    candidate = m.group(1) + ".png"
    if candidate in GOLDEN_FIXTURE_FILES:
        return candidate
    return None


# ── OCR via macOS Vision ─────────────────────────────────────────────────────

def ocr_image(image_path: Path) -> list[dict]:
    """Run macOS Vision OCR on an image, return list of {text, bbox [x,y,w,h]}."""
    try:
        import Vision
        import Quartz
    except ImportError:
        print("WARN: pyobjc-framework-Vision not available, skipping OCR", file=sys.stderr)
        return []

    image_url = Quartz.CFURLCreateWithFileSystemPath(
        None, str(image_path), Quartz.kCFURLPOSIXPathStyle, False
    )
    image_source = Quartz.CGImageSourceCreateWithURL(image_url, None)
    if image_source is None:
        print(f"  WARN: cannot load image for OCR: {image_path}", file=sys.stderr)
        return []
    cg_image = Quartz.CGImageSourceCreateImageAtIndex(image_source, 0, None)
    if cg_image is None:
        return []

    img_w = Quartz.CGImageGetWidth(cg_image)
    img_h = Quartz.CGImageGetHeight(cg_image)

    handler = Vision.VNImageRequestHandler.alloc().initWithCGImage_options_(cg_image, None)
    request = Vision.VNRecognizeTextRequest.alloc().init()
    request.setRecognitionLevel_(Vision.VNRequestTextRecognitionLevelAccurate)

    success, error = handler.performRequests_error_([request], None)
    if not success:
        print(f"  WARN: OCR failed: {error}", file=sys.stderr)
        return []

    results = []
    for obs in request.results():
        candidates = obs.topCandidates_(1)
        if not candidates:
            continue
        text = candidates[0].string()
        if not text or not text.strip():
            continue
        bb = obs.boundingBox()
        # Vision uses normalized coords with origin at bottom-left
        x = bb.origin.x * img_w
        y = (1.0 - bb.origin.y - bb.size.height) * img_h
        w = bb.size.width * img_w
        h = bb.size.height * img_h
        results.append({"text": text.strip(), "bbox": [x, y, w, h]})
    return results


def ocr_text_in_bbox(ocr_results: list[dict], bbox: list[float]) -> str:
    """Find OCR text whose center falls inside the given bbox."""
    bx, by, bw, bh = bbox
    texts = []
    for ocr in ocr_results:
        ox, oy, ow, oh = ocr["bbox"]
        cx = ox + ow / 2
        cy = oy + oh / 2
        if bx <= cx <= bx + bw and by <= cy <= by + bh:
            texts.append(ocr["text"])
    return " ".join(texts)


# ── main ─────────────────────────────────────────────────────────────────────

def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    p.add_argument("--project-id", type=int, default=6)
    p.add_argument("--pg-host", default="localhost")
    p.add_argument("--pg-port", type=int, default=5432)
    p.add_argument("--pg-user", default=os.getenv("POSTGRES_USER", "desktopctl"))
    p.add_argument("--pg-password", default=os.getenv("POSTGRES_PASSWORD", "desktopctl"))
    p.add_argument("--data-dir", default=os.getenv("DATA_DIR"))
    p.add_argument("--output", default=str(OUTPUT_PATH))
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

    # Group by fixture filename, filter to golden fixture images
    image_controls: dict[str, dict] = {}
    ocr_cache: dict[str, list[dict]] = {}

    for task in tasks:
        data = task["data"]
        result = task["result"]

        label_id = data.get("label_id", f"task_{task['task_id']}")
        fixture_file = item_id_to_fixture_filename(label_id)
        if fixture_file is None:
            continue

        image_url = data.get("image", "")
        image_path = ls_url_to_path(image_url, data_dir)

        # Collect control annotations
        controls = {"text_fields": [], "buttons": []}
        for ann in result:
            if ann.get("type") != "rectanglelabels":
                continue
            value = ann.get("value", {})
            labels = value.get("rectanglelabels", [])
            if not labels:
                continue
            category = labels[0]
            if category not in CONTROL_CATEGORIES:
                continue
            bbox = convert_rect(ann)
            key = "text_fields" if category == "text_field" else "buttons"
            controls[key].append({"bbox": bbox, "text": ""})

        if not controls["text_fields"] and not controls["buttons"]:
            continue

        # Run OCR for text labels
        if fixture_file not in ocr_cache:
            # Prefer fixture image, fall back to LS image path
            fixture_path = FIXTURE_DIR / fixture_file
            ocr_path = fixture_path if fixture_path.exists() else image_path
            if ocr_path.exists():
                print(f"  OCR: {ocr_path.name}")
                ocr_cache[fixture_file] = ocr_image(ocr_path)
            else:
                print(f"  WARN: no image for OCR: {fixture_file}", file=sys.stderr)
                ocr_cache[fixture_file] = []

        ocr_results = ocr_cache[fixture_file]
        for control_list in (controls["text_fields"], controls["buttons"]):
            for ctrl in control_list:
                ctrl["text"] = ocr_text_in_bbox(ocr_results, ctrl["bbox"])

        image_controls[fixture_file] = {
            "file": fixture_file,
            **controls,
        }

    # Build output sorted by filename
    output = [image_controls[k] for k in sorted(image_controls)]

    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(json.dumps(output, indent=2))

    print(f"\n  exported: {len(output)} images")
    print(f"  output:   {output_path}")
    for entry in output:
        tf = len(entry["text_fields"])
        btn = len(entry["buttons"])
        print(f"    {entry['file']}: {tf} text_fields, {btn} buttons")


if __name__ == "__main__":
    main()
