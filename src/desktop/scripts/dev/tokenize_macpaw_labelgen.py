#!/usr/bin/env python3
"""Generate tokenize labels using local MacPaw YOLO11l models."""

from __future__ import annotations

import argparse
import json
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import cv2  # type: ignore

import tokenize_label_corpus as tlc


IMAGE_SUFFIXES = {".png", ".jpg", ".jpeg"}


@dataclass
class Detection:
    rect: tlc.Rect
    score: float
    source: str


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Build auto labels from MacPaw YOLO11l UI models."
    )
    parser.add_argument("--input", required=True, help="Input screenshot directory (recursive).")
    parser.add_argument("--output", required=True, help="Output directory for labels/overlays.")
    parser.add_argument(
        "--elements-weights",
        default="/Users/oleg/Projects/DesktopCtl/tmp/macpaw/yolov11l-ui-elements-detection/ui-elements-detection.pt",
        help="Path to MacPaw UI elements model weights (.pt).",
    )
    parser.add_argument(
        "--groups-weights",
        default="/Users/oleg/Projects/DesktopCtl/tmp/macpaw/yolov11l-ui-groups-detection/ui-groups-detection.pt",
        help="Path to MacPaw UI groups model weights (.pt).",
    )
    parser.add_argument(
        "--disable-elements",
        action="store_true",
        help="Disable elements model (run groups only).",
    )
    parser.add_argument(
        "--disable-groups",
        action="store_true",
        help="Disable groups model (run elements only).",
    )
    parser.add_argument(
        "--elements-conf",
        type=float,
        default=0.22,
        help="Confidence threshold for elements model.",
    )
    parser.add_argument(
        "--groups-conf",
        type=float,
        default=0.20,
        help="Confidence threshold for groups model.",
    )
    parser.add_argument(
        "--iou",
        type=float,
        default=0.40,
        help="NMS IoU threshold used by model inference.",
    )
    parser.add_argument(
        "--imgsz",
        type=int,
        default=1280,
        help="YOLO inference image size.",
    )
    parser.add_argument(
        "--device",
        default="auto",
        choices=["auto", "cpu", "mps", "cuda"],
        help="YOLO device.",
    )
    parser.add_argument(
        "--text-mode",
        default="any",
        choices=["any", "overlap", "center"],
        help="How to keep model boxes relative to OCR text.",
    )
    parser.add_argument(
        "--text-overlap-min",
        type=float,
        default=0.03,
        help="Min IoU overlap with text when --text-mode overlap is used.",
    )
    parser.add_argument(
        "--lang",
        default="eng",
        help="Tesseract language.",
    )
    parser.add_argument(
        "--skip-glyphs",
        action="store_true",
        help="Disable glyph extraction.",
    )
    parser.add_argument(
        "--elements-max-rel-area",
        type=float,
        default=0.65,
        help="Reject element boxes larger than this fraction of image area.",
    )
    parser.add_argument(
        "--groups-max-rel-area",
        type=float,
        default=0.95,
        help="Reject group boxes larger than this fraction of image area.",
    )
    parser.add_argument(
        "--min-size",
        type=int,
        default=8,
        help="Reject boxes smaller than this size in either dimension.",
    )
    parser.add_argument(
        "--dedupe-iou",
        type=float,
        default=0.92,
        help="IoU threshold for deduping predictions.",
    )
    parser.add_argument(
        "--max-boxes",
        type=int,
        default=700,
        help="Max number of box elements per image after dedupe.",
    )
    parser.add_argument(
        "--write-overlays",
        action="store_true",
        help="Write .overlay.png files.",
    )
    return parser.parse_args()


def iter_images(root: Path) -> list[Path]:
    return [
        path
        for path in sorted(root.rglob("*"))
        if path.is_file() and path.suffix.lower() in IMAGE_SUFFIXES
    ]


def resolve_device(token: str) -> str:
    if token != "auto":
        return token
    try:
        import torch  # type: ignore
    except Exception:
        return "cpu"
    if getattr(torch.backends, "mps", None) and torch.backends.mps.is_available():
        return "mps"
    if torch.cuda.is_available():
        return "cuda"
    return "cpu"


def to_rect(bbox: list[int]) -> tlc.Rect:
    return tlc.Rect(x=bbox[0], y=bbox[1], w=bbox[2], h=bbox[3])


def overlaps_text(
    rect: tlc.Rect,
    text_elements: list[dict[str, Any]],
    mode: str,
    overlap_min: float,
) -> bool:
    if mode == "any":
        return True
    if not text_elements:
        return False
    if mode == "center":
        cx = rect.x + rect.w / 2.0
        cy = rect.y + rect.h / 2.0
        for element in text_elements:
            tx, ty, tw, th = element["bbox"]
            if tx <= cx <= tx + tw and ty <= cy <= ty + th:
                return True
        return False
    for element in text_elements:
        if tlc.iou(rect, to_rect(element["bbox"])) >= overlap_min:
            return True
    return False


def dedupe_detections(detections: list[Detection], iou_threshold: float) -> list[Detection]:
    ranked = sorted(detections, key=lambda d: d.score, reverse=True)
    kept: list[Detection] = []
    for candidate in ranked:
        if any(tlc.iou(candidate.rect, existing.rect) > iou_threshold for existing in kept):
            continue
        kept.append(candidate)
    kept.sort(key=lambda d: (d.rect.y, d.rect.x, d.rect.w * d.rect.h))
    return kept


def run_yolo(
    model: Any,
    image_path: Path,
    image_shape: tuple[int, int],
    *,
    conf: float,
    iou: float,
    imgsz: int,
    device: str,
    source_prefix: str,
    min_size: int,
    max_rel_area: float,
) -> list[Detection]:
    h, w = image_shape
    img_area = float(max(1, h * w))
    results = model.predict(
        source=str(image_path),
        conf=conf,
        iou=iou,
        imgsz=imgsz,
        device=device,
        verbose=False,
        max_det=1500,
    )
    if not results:
        return []
    boxes = results[0].boxes
    if boxes is None or boxes.xyxy is None:
        return []
    xyxy = boxes.xyxy.detach().cpu().numpy()
    confs = boxes.conf.detach().cpu().numpy() if boxes.conf is not None else None
    classes = boxes.cls.detach().cpu().numpy() if boxes.cls is not None else None
    names = getattr(model, "names", {}) or {}

    out: list[Detection] = []
    for idx in range(xyxy.shape[0]):
        x1, y1, x2, y2 = [int(round(float(v))) for v in xyxy[idx]]
        rect = tlc.Rect(x=x1, y=y1, w=max(0, x2 - x1), h=max(0, y2 - y1)).clipped(w, h)
        if rect is None:
            continue
        if rect.w < min_size or rect.h < min_size:
            continue
        if (rect.w * rect.h) / img_area > max_rel_area:
            continue
        score = float(confs[idx]) if confs is not None else 1.0
        class_name = "unknown"
        if classes is not None:
            class_id = int(classes[idx])
            class_name = str(names.get(class_id, class_id))
        out.append(
            Detection(
                rect=rect,
                score=score,
                source=f"{source_prefix}:{class_name}",
            )
        )
    return out


def detections_to_box_elements(detections: list[Detection]) -> list[dict[str, Any]]:
    elements: list[dict[str, Any]] = []
    for idx, detection in enumerate(detections, start=1):
        elements.append(
            {
                "id": f"box_{idx:04d}",
                "type": "box",
                "bbox": detection.rect.as_list(),
                "confidence": round(detection.score, 4),
                "source": detection.source,
            }
        )
    return elements


def main() -> int:
    args = parse_args()
    input_root = Path(args.input).expanduser().resolve()
    output_root = Path(args.output).expanduser().resolve()
    output_root.mkdir(parents=True, exist_ok=True)

    images = iter_images(input_root)
    if not images:
        print(f"No images found under: {input_root}")
        return 1

    if args.disable_elements and args.disable_groups:
        raise SystemExit("Both models are disabled. Enable at least one model.")

    try:
        from ultralytics import YOLO  # type: ignore
    except ModuleNotFoundError as exc:
        raise SystemExit(
            "ultralytics is not installed for this Python environment. "
            "Use scripts/.venv and install dependencies first."
        ) from exc

    device = resolve_device(args.device)
    elements_model = None
    groups_model = None
    if not args.disable_elements:
        if not Path(args.elements_weights).exists():
            raise SystemExit(f"elements weights not found: {args.elements_weights}")
        elements_model = YOLO(args.elements_weights)
    if not args.disable_groups:
        if not Path(args.groups_weights).exists():
            raise SystemExit(f"groups weights not found: {args.groups_weights}")
        groups_model = YOLO(args.groups_weights)

    generated = 0
    failed = 0
    boxes_total = 0
    failures: list[dict[str, str]] = []

    for image_path in images:
        rel = image_path.relative_to(input_root)
        out_dir = output_root / rel.parent
        out_dir.mkdir(parents=True, exist_ok=True)
        label_path = out_dir / f"{image_path.stem}.labels.json"
        overlay_path = out_dir / f"{image_path.stem}.overlay.png"

        try:
            image = cv2.imread(str(image_path))
            if image is None:
                raise RuntimeError("failed to decode image")
            h, w = image.shape[:2]

            rows = tlc.run_tesseract_tsv(image_path, lang=args.lang)
            text_elements = tlc.extract_text_elements(rows, width=w, height=h)
            glyph_elements = [] if args.skip_glyphs else tlc.detect_glyphs(image, text_elements)

            detections: list[Detection] = []
            if elements_model is not None:
                detections.extend(
                    run_yolo(
                        elements_model,
                        image_path,
                        (h, w),
                        conf=args.elements_conf,
                        iou=args.iou,
                        imgsz=args.imgsz,
                        device=device,
                        source_prefix="macpaw:yolo11l_elements",
                        min_size=args.min_size,
                        max_rel_area=args.elements_max_rel_area,
                    )
                )
            if groups_model is not None:
                detections.extend(
                    run_yolo(
                        groups_model,
                        image_path,
                        (h, w),
                        conf=args.groups_conf,
                        iou=args.iou,
                        imgsz=args.imgsz,
                        device=device,
                        source_prefix="macpaw:yolo11l_groups",
                        min_size=args.min_size,
                        max_rel_area=args.groups_max_rel_area,
                    )
                )

            filtered = [
                det
                for det in detections
                if overlaps_text(det.rect, text_elements, args.text_mode, args.text_overlap_min)
            ]
            deduped = dedupe_detections(filtered, args.dedupe_iou)[: args.max_boxes]
            box_elements = detections_to_box_elements(deduped)
            boxes_total += len(box_elements)

            payload = tlc.make_payload(
                image_path=image_path,
                image=image,
                text_elements=text_elements,
                box_elements=box_elements,
                glyph_elements=glyph_elements,
                box_detector="macpaw_yolo11l",
            )
            payload.setdefault("meta", {})["source"] = "tesseract+macpaw:yolo11l_elements_groups"
            payload["meta"]["models"] = {
                "elements": None if args.disable_elements else str(Path(args.elements_weights)),
                "groups": None if args.disable_groups else str(Path(args.groups_weights)),
            }
            payload["meta"]["params"] = {
                "elements_conf": args.elements_conf,
                "groups_conf": args.groups_conf,
                "iou": args.iou,
                "imgsz": args.imgsz,
                "device": device,
                "text_mode": args.text_mode,
                "text_overlap_min": args.text_overlap_min,
                "elements_max_rel_area": args.elements_max_rel_area,
                "groups_max_rel_area": args.groups_max_rel_area,
                "dedupe_iou": args.dedupe_iou,
                "max_boxes": args.max_boxes,
            }

            label_path.write_text(json.dumps(payload, indent=2), encoding="utf-8")
            if args.write_overlays:
                overlay = tlc.draw_overlay(image, payload)
                cv2.imwrite(str(overlay_path), overlay)
            generated += 1
        except Exception as exc:  # pragma: no cover - runtime dependent
            failed += 1
            if len(failures) < 30:
                failures.append({"image": str(rel), "error": repr(exc)})

    summary = {
        "input_root": str(input_root),
        "output_root": str(output_root),
        "images_total": len(images),
        "images_generated": generated,
        "images_failed": failed,
        "boxes_total": boxes_total,
        "boxes_avg_per_image": round(boxes_total / max(1, generated), 3),
        "failures": failures,
    }
    (output_root / "_summary.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(json.dumps(summary, indent=2))
    return 0 if failed == 0 else 2


if __name__ == "__main__":
    raise SystemExit(main())
