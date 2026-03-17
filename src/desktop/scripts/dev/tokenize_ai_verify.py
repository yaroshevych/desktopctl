#!/usr/bin/env python3
"""Heuristic verifier for auto-generated tokenize labels."""

from __future__ import annotations

import argparse
import json
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import cv2  # type: ignore


@dataclass(frozen=True)
class Rect:
    x: int
    y: int
    w: int
    h: int

    def as_list(self) -> list[int]:
        return [self.x, self.y, self.w, self.h]

    def clipped(self, width: int, height: int) -> Rect | None:
        x1 = max(0, min(width, self.x))
        y1 = max(0, min(height, self.y))
        x2 = max(0, min(width, self.x + self.w))
        y2 = max(0, min(height, self.y + self.h))
        if x2 <= x1 or y2 <= y1:
            return None
        return Rect(x=x1, y=y1, w=x2 - x1, h=y2 - y1)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Verify/correct auto tokenize labels and assign verdicts."
    )
    parser.add_argument("--input", required=True, help="Input directory of *.labels.json files.")
    parser.add_argument("--output", required=True, help="Output directory for verified JSON.")
    parser.add_argument(
        "--write-overlays",
        action="store_true",
        help="Write verification overlays with verdict header.",
    )
    return parser.parse_args()


def iter_label_files(root: Path) -> list[Path]:
    return sorted(root.rglob("*.labels.json"))


def iou(a: Rect, b: Rect) -> float:
    ix1 = max(a.x, b.x)
    iy1 = max(a.y, b.y)
    ix2 = min(a.x + a.w, b.x + b.w)
    iy2 = min(a.y + a.h, b.y + b.h)
    iw = max(0, ix2 - ix1)
    ih = max(0, iy2 - iy1)
    inter = iw * ih
    if inter == 0:
        return 0.0
    union = a.w * a.h + b.w * b.h - inter
    return inter / max(1, union)


def normalize_elements(payload: dict[str, Any]) -> tuple[int, list[str]]:
    issues: list[str] = []
    corrected = 0
    image = payload.get("image", {})
    width = int(image.get("width", 0))
    height = int(image.get("height", 0))
    if width <= 0 or height <= 0:
        issues.append("invalid_image_size")
        return corrected, issues

    for window in payload.get("windows", []):
        normalized: list[dict[str, Any]] = []
        for element in window.get("elements", []):
            bbox = element.get("bbox", [])
            if not isinstance(bbox, list) or len(bbox) != 4:
                corrected += 1
                continue
            try:
                rect = Rect(
                    x=int(round(float(bbox[0]))),
                    y=int(round(float(bbox[1]))),
                    w=int(round(float(bbox[2]))),
                    h=int(round(float(bbox[3]))),
                )
            except (TypeError, ValueError):
                corrected += 1
                continue
            clip = rect.clipped(width, height)
            if clip is None:
                corrected += 1
                continue
            if clip != rect:
                corrected += 1
            element["bbox"] = clip.as_list()
            normalized.append(element)

        # Deduplicate near-identical boxes by type.
        deduped: list[dict[str, Any]] = []
        for element in sorted(normalized, key=lambda e: (e["bbox"][1], e["bbox"][0], e["id"])):
            rect = Rect(*element["bbox"])
            duplicate = False
            for existing in deduped:
                if existing.get("type") != element.get("type"):
                    continue
                if iou(rect, Rect(*existing["bbox"])) > 0.96:
                    duplicate = True
                    corrected += 1
                    break
            if not duplicate:
                deduped.append(element)
        window["elements"] = deduped

    if corrected > 0:
        issues.append("auto_corrections_applied")
    return corrected, issues


def verdict_for(payload: dict[str, Any], corrections: int, issues: list[str]) -> tuple[str, list[str]]:
    elements = payload.get("windows", [{}])[0].get("elements", [])
    text_count = sum(1 for e in elements if e.get("type") == "text")
    box_count = sum(1 for e in elements if e.get("type") == "box")
    glyph_count = sum(1 for e in elements if e.get("type") == "glyph")

    reasons = list(issues)
    verdict = "accept"
    if text_count < 2:
        verdict = "needs_human_review"
        reasons.append("too_few_text_tokens")
    if box_count < max(1, text_count // 4):
        verdict = "needs_human_review"
        reasons.append("box_coverage_low")
    if glyph_count > 140:
        verdict = "needs_human_review"
        reasons.append("glyph_count_high")
    if corrections > 20:
        verdict = "needs_human_review"
        reasons.append("many_auto_corrections")
    return verdict, sorted(set(reasons))


def render_overlay(image_path: Path, payload: dict[str, Any], verdict: str, reasons: list[str], out_path: Path) -> None:
    image = cv2.imread(str(image_path))
    if image is None:
        return
    for window in payload.get("windows", []):
        for element in window.get("elements", []):
            x, y, w, h = [int(v) for v in element.get("bbox", [0, 0, 0, 0])]
            if element.get("type") == "text":
                color = (0, 180, 0)
            elif element.get("type") == "box":
                color = (220, 120, 20)
            else:
                color = (0, 170, 220)
            cv2.rectangle(image, (x, y), (x + w, y + h), color, 1)
    title = f"verdict={verdict}"
    subtitle = ",".join(reasons[:3]) if reasons else "ok"
    cv2.putText(image, title, (12, 24), cv2.FONT_HERSHEY_SIMPLEX, 0.7, (255, 255, 255), 2)
    cv2.putText(image, subtitle, (12, 48), cv2.FONT_HERSHEY_SIMPLEX, 0.5, (200, 200, 200), 1)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    cv2.imwrite(str(out_path), image)


def main() -> int:
    args = parse_args()
    input_root = Path(args.input).expanduser().resolve()
    output_root = Path(args.output).expanduser().resolve()
    output_root.mkdir(parents=True, exist_ok=True)

    label_files = iter_label_files(input_root)
    if not label_files:
        print(f"No *.labels.json files under {input_root}")
        return 1

    counts = {"accept": 0, "needs_human_review": 0}
    for path in label_files:
        rel = path.relative_to(input_root)
        out_json = output_root / rel
        out_json.parent.mkdir(parents=True, exist_ok=True)

        payload = json.loads(path.read_text(encoding="utf-8"))
        corrections, issues = normalize_elements(payload)
        verdict, reasons = verdict_for(payload, corrections, issues)
        counts[verdict] += 1
        payload["qa"] = {
            "labeler": "ai",
            "label_version": "v1",
            "verified_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
            "verdict": verdict,
            "issues": reasons,
            "auto_corrections": corrections,
        }
        out_json.write_text(json.dumps(payload, indent=2), encoding="utf-8")

        if args.write_overlays:
            image_path = Path(payload.get("image", {}).get("path", ""))
            if image_path.exists():
                overlay_path = output_root / rel.with_suffix(".qa.overlay.png")
                render_overlay(image_path, payload, verdict, reasons, overlay_path)

    print(
        json.dumps(
            {
                "input_root": str(input_root),
                "output_root": str(output_root),
                "files_total": len(label_files),
                "accept": counts["accept"],
                "needs_human_review": counts["needs_human_review"],
            },
            indent=2,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
