#!/usr/bin/env python3
"""Generate primitive tokenize labels (text/box/glyph) for screenshot corpus."""

from __future__ import annotations

import argparse
import json
import subprocess
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import cv2  # type: ignore
import numpy as np


IMAGE_SUFFIXES = {".png", ".jpg", ".jpeg"}


@dataclass(frozen=True)
class Rect:
    x: int
    y: int
    w: int
    h: int

    def as_list(self) -> list[int]:
        return [self.x, self.y, self.w, self.h]

    @property
    def x2(self) -> int:
        return self.x + self.w

    @property
    def y2(self) -> int:
        return self.y + self.h

    def clipped(self, width: int, height: int) -> Rect | None:
        x1 = max(0, min(width, self.x))
        y1 = max(0, min(height, self.y))
        x2 = max(0, min(width, self.x2))
        y2 = max(0, min(height, self.y2))
        if x2 <= x1 or y2 <= y1:
            return None
        return Rect(x=x1, y=y1, w=x2 - x1, h=y2 - y1)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Build auto labels for tokenize corpus (text/box/glyph)."
    )
    parser.add_argument(
        "--input",
        required=True,
        help="Input directory containing screenshot PNG/JPG files (recursive).",
    )
    parser.add_argument(
        "--output",
        required=True,
        help="Output directory for label JSON and overlays.",
    )
    parser.add_argument(
        "--write-overlays",
        action="store_true",
        help="Write overlay PNG files next to labels.",
    )
    parser.add_argument(
        "--box-padding",
        type=int,
        default=6,
        help="Padding in pixels for text-anchored candidate boxes.",
    )
    parser.add_argument(
        "--lang",
        default="eng",
        help="Tesseract OCR language (default: eng).",
    )
    parser.add_argument(
        "--skip-glyphs",
        action="store_true",
        help="Disable glyph detection.",
    )
    return parser.parse_args()


def iter_images(root: Path) -> list[Path]:
    images: list[Path] = []
    for path in sorted(root.rglob("*")):
        if path.is_file() and path.suffix.lower() in IMAGE_SUFFIXES:
            images.append(path)
    return images


def run_tesseract_tsv(image_path: Path, lang: str) -> list[dict[str, str]]:
    cmd = [
        "tesseract",
        str(image_path),
        "stdout",
        "--psm",
        "6",
        "-l",
        lang,
        "tsv",
    ]
    proc = subprocess.run(cmd, text=True, capture_output=True, check=False)
    if proc.returncode != 0:
        raise RuntimeError(
            f"tesseract failed for {image_path}: {proc.stderr.strip() or proc.stdout.strip()}"
        )
    lines = proc.stdout.splitlines()
    if not lines:
        return []
    header = lines[0].split("\t")
    rows: list[dict[str, str]] = []
    for line in lines[1:]:
        parts = line.split("\t")
        if len(parts) != len(header):
            continue
        row = {header[i]: parts[i] for i in range(len(header))}
        rows.append(row)
    return rows


def extract_text_elements(rows: list[dict[str, str]], width: int, height: int) -> list[dict[str, Any]]:
    elements: list[dict[str, Any]] = []
    next_id = 1
    for row in rows:
        text = row.get("text", "").strip()
        if not text:
            continue
        try:
            conf = float(row.get("conf", "-1"))
            x = int(row.get("left", "0"))
            y = int(row.get("top", "0"))
            w = int(row.get("width", "0"))
            h = int(row.get("height", "0"))
        except ValueError:
            continue
        if conf < 0 or w <= 1 or h <= 1:
            continue
        rect = Rect(x=x, y=y, w=w, h=h).clipped(width, height)
        if rect is None:
            continue
        elements.append(
            {
                "id": f"text_{next_id:04d}",
                "type": "text",
                "bbox": rect.as_list(),
                "text": text,
                "confidence": round(conf / 100.0, 4),
                "source": "tesseract",
            }
        )
        next_id += 1
    elements.sort(key=lambda e: (e["bbox"][1], e["bbox"][0], e["id"]))
    return elements


def iou(a: Rect, b: Rect) -> float:
    ix1 = max(a.x, b.x)
    iy1 = max(a.y, b.y)
    ix2 = min(a.x2, b.x2)
    iy2 = min(a.y2, b.y2)
    iw = max(0, ix2 - ix1)
    ih = max(0, iy2 - iy1)
    inter = iw * ih
    if inter == 0:
        return 0.0
    union = a.w * a.h + b.w * b.h - inter
    return inter / max(1, union)


def box_from_text(text_elements: list[dict[str, Any]], width: int, height: int, padding: int) -> list[dict[str, Any]]:
    boxes: list[dict[str, Any]] = []
    next_id = 1
    for text_el in text_elements:
        x, y, w, h = text_el["bbox"]
        rect = Rect(x=x - padding, y=y - padding, w=w + 2 * padding, h=h + 2 * padding).clipped(
            width, height
        )
        if rect is None:
            continue
        duplicate = False
        for existing in boxes:
            ex = Rect(*existing["bbox"])
            if iou(ex, rect) > 0.95:
                duplicate = True
                break
        if duplicate:
            continue
        boxes.append(
            {
                "id": f"box_{next_id:04d}",
                "type": "box",
                "bbox": rect.as_list(),
                "confidence": 0.6,
                "source": "text_anchor_padding",
            }
        )
        next_id += 1
    boxes.sort(key=lambda e: (e["bbox"][1], e["bbox"][0], e["id"]))
    return boxes


def overlap_ratio(a: Rect, b: Rect) -> float:
    ix1 = max(a.x, b.x)
    iy1 = max(a.y, b.y)
    ix2 = min(a.x2, b.x2)
    iy2 = min(a.y2, b.y2)
    iw = max(0, ix2 - ix1)
    ih = max(0, iy2 - iy1)
    inter = iw * ih
    if inter == 0:
        return 0.0
    return inter / max(1, a.w * a.h)


def detect_glyphs(image: np.ndarray, text_elements: list[dict[str, Any]]) -> list[dict[str, Any]]:
    gray = cv2.cvtColor(image, cv2.COLOR_BGR2GRAY)
    blur = cv2.GaussianBlur(gray, (3, 3), 0)
    bin_img = cv2.adaptiveThreshold(
        blur,
        255,
        cv2.ADAPTIVE_THRESH_GAUSSIAN_C,
        cv2.THRESH_BINARY_INV,
        15,
        4,
    )
    num_labels, labels, stats, _centroids = cv2.connectedComponentsWithStats(bin_img, connectivity=8)
    text_rects = [Rect(*el["bbox"]) for el in text_elements]
    glyphs: list[dict[str, Any]] = []
    next_id = 1
    for i in range(1, num_labels):
        x, y, w, h, area = [int(v) for v in stats[i]]
        if area < 18 or area > 1800:
            continue
        if w <= 1 or h <= 1 or w > 80 or h > 80:
            continue
        aspect = w / max(1, h)
        if aspect < 0.15 or aspect > 6.0:
            continue
        rect = Rect(x=x, y=y, w=w, h=h)
        if any(overlap_ratio(rect, t) > 0.2 for t in text_rects):
            continue
        glyphs.append(
            {
                "id": f"glyph_{next_id:04d}",
                "type": "glyph",
                "bbox": rect.as_list(),
                "confidence": 0.5,
                "source": "connected_components",
            }
        )
        next_id += 1
    glyphs.sort(key=lambda e: (e["bbox"][1], e["bbox"][0], e["id"]))
    return glyphs[:120]


def parse_optional_bounds_json(bounds_path: Path) -> dict[str, Any] | None:
    if not bounds_path.exists():
        return None
    try:
        payload = json.loads(bounds_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return None
    if not isinstance(payload, dict):
        return None
    window = payload.get("window")
    if isinstance(window, dict):
        return window
    return None


def make_payload(
    image_path: Path,
    image: np.ndarray,
    text_elements: list[dict[str, Any]],
    box_elements: list[dict[str, Any]],
    glyph_elements: list[dict[str, Any]],
) -> dict[str, Any]:
    height, width = image.shape[:2]
    stem = image_path.stem
    bounds_sidecar = image_path.with_name(f"{stem}.bounds.json")
    sidecar_window = parse_optional_bounds_json(bounds_sidecar)

    window_title = image_path.parent.name
    window_bounds = [0, 0, width, height]
    if sidecar_window:
        wx = int(round(float(sidecar_window.get("bounds", {}).get("x", 0.0))))
        wy = int(round(float(sidecar_window.get("bounds", {}).get("y", 0.0))))
        ww = int(round(float(sidecar_window.get("bounds", {}).get("width", width))))
        wh = int(round(float(sidecar_window.get("bounds", {}).get("height", height))))
        window_bounds = [wx, wy, ww, wh]
        window_title = str(sidecar_window.get("title") or sidecar_window.get("app") or window_title)

    elements = text_elements + box_elements + glyph_elements
    elements.sort(key=lambda e: (e["bbox"][1], e["bbox"][0], e["id"]))
    return {
        "image": {
            "path": str(image_path),
            "width": width,
            "height": height,
        },
        "windows": [
            {
                "id": "win_0001",
                "title": window_title,
                "bounds": window_bounds,
                "elements": elements,
            }
        ],
        "meta": {
            "generated_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
            "labeler": "auto",
            "label_version": "v1",
            "source": "tesseract+opencv",
        },
    }


def draw_overlay(image: np.ndarray, payload: dict[str, Any]) -> np.ndarray:
    canvas = image.copy()
    for window in payload.get("windows", []):
        wb = window.get("bounds", [0, 0, image.shape[1], image.shape[0]])
        wx, wy, ww, wh = [int(v) for v in wb]
        cv2.rectangle(canvas, (wx, wy), (wx + ww, wy + wh), (255, 255, 255), 2)
        for element in window.get("elements", []):
            x, y, w, h = [int(v) for v in element.get("bbox", [0, 0, 0, 0])]
            etype = element.get("type")
            if etype == "text":
                color = (0, 180, 0)
            elif etype == "box":
                color = (200, 90, 20)
            else:
                color = (0, 180, 200)
            cv2.rectangle(canvas, (x, y), (x + w, y + h), color, 1)
    return canvas


def main() -> int:
    args = parse_args()
    input_root = Path(args.input).expanduser().resolve()
    output_root = Path(args.output).expanduser().resolve()
    output_root.mkdir(parents=True, exist_ok=True)

    images = iter_images(input_root)
    if not images:
        print(f"No images found under: {input_root}")
        return 1

    generated = 0
    skipped = 0
    for image_path in images:
        rel = image_path.relative_to(input_root)
        out_dir = output_root / rel.parent
        out_dir.mkdir(parents=True, exist_ok=True)
        label_path = out_dir / f"{image_path.stem}.labels.json"
        overlay_path = out_dir / f"{image_path.stem}.overlay.png"

        image = cv2.imread(str(image_path))
        if image is None:
            skipped += 1
            continue
        h, w = image.shape[:2]
        rows = run_tesseract_tsv(image_path, lang=args.lang)
        text_elements = extract_text_elements(rows, width=w, height=h)
        box_elements = box_from_text(text_elements, width=w, height=h, padding=max(0, args.box_padding))
        glyph_elements = [] if args.skip_glyphs else detect_glyphs(image, text_elements)
        payload = make_payload(image_path, image, text_elements, box_elements, glyph_elements)
        label_path.write_text(json.dumps(payload, indent=2), encoding="utf-8")
        if args.write_overlays:
            overlay = draw_overlay(image, payload)
            cv2.imwrite(str(overlay_path), overlay)
        generated += 1

    print(
        json.dumps(
            {
                "input_root": str(input_root),
                "output_root": str(output_root),
                "images_total": len(images),
                "labels_generated": generated,
                "images_skipped": skipped,
            },
            indent=2,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
