#!/usr/bin/env python3
"""Benchmark multiple box detectors for tokenize corpus and write comparable overlays."""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import cv2  # type: ignore
import numpy as np

import tokenize_label_corpus as tlc


SUPPORTED_MODELS = [
    "cv_edge_rect",
    "cv_edge_ellipse",
    "cv_morph_gradient",
    "omniparser_icon",
    "yolov8n",
    "rtdetr_r18",
    "table_transformer",
    "grounding_dino",
]

DEFAULT_GROUNDING_PROMPT = (
    "button . icon . input field . list item . panel . table cell . grid cell . toolbar ."
)
MODEL_INIT_LOCK = threading.Lock()


@dataclass
class Detection:
    rect: tlc.Rect
    score: float
    source: str


class AdapterBase:
    def detect(
        self,
        image_path: Path,
        image_bgr: np.ndarray,
        text_elements: list[dict[str, Any]],
    ) -> list[Detection]:
        raise NotImplementedError


class CvAdapter(AdapterBase):
    def __init__(self, detector: str):
        self.detector = detector

    def detect(
        self,
        image_path: Path,
        image_bgr: np.ndarray,
        text_elements: list[dict[str, Any]],
    ) -> list[Detection]:
        rects, source, score = tlc.detect_box_rects(image_bgr, text_elements, detector=self.detector)
        return [Detection(rect=r, score=score, source=f"cv:{source}") for r in rects]


class YoloAdapter(AdapterBase):
    def __init__(self, *, weights: str, source_name: str, device: str, conf: float):
        import torch  # type: ignore
        from ultralytics import YOLO  # type: ignore

        self.model = YOLO(weights)
        self.source_name = source_name
        self.device = resolve_yolo_device(device, torch)
        self.conf = conf

    def detect(
        self,
        image_path: Path,
        image_bgr: np.ndarray,
        text_elements: list[dict[str, Any]],
    ) -> list[Detection]:
        results = self.model.predict(
            source=str(image_path),
            conf=self.conf,
            iou=0.35,
            imgsz=1280,
            device=self.device,
            verbose=False,
            max_det=1200,
        )
        if not results:
            return []
        boxes = results[0].boxes
        if boxes is None or boxes.xyxy is None:
            return []

        xyxy = boxes.xyxy.detach().cpu().numpy()
        conf = boxes.conf.detach().cpu().numpy() if boxes.conf is not None else np.ones((xyxy.shape[0],))
        h, w = image_bgr.shape[:2]
        detections: list[Detection] = []
        for idx in range(xyxy.shape[0]):
            x1, y1, x2, y2 = [int(round(float(v))) for v in xyxy[idx]]
            rect = tlc.Rect(x=x1, y=y1, w=max(0, x2 - x1), h=max(0, y2 - y1)).clipped(w, h)
            if rect is None:
                continue
            if rect.w < 6 or rect.h < 6:
                continue
            detections.append(
                Detection(
                    rect=rect,
                    score=float(conf[idx]),
                    source=self.source_name,
                )
            )
        return dedupe_detections(detections, iou_threshold=0.95)


class HfObjectDetectionAdapter(AdapterBase):
    def __init__(
        self,
        *,
        model_id: str,
        source_name: str,
        threshold: float,
        device: str,
        allow_download: bool,
    ):
        import torch  # type: ignore
        from transformers import AutoImageProcessor, AutoModelForObjectDetection  # type: ignore

        self.torch = torch
        self.processor = AutoImageProcessor.from_pretrained(model_id, local_files_only=not allow_download)
        self.model = AutoModelForObjectDetection.from_pretrained(model_id, local_files_only=not allow_download)
        self.device = resolve_torch_device(torch, device)
        self.model.to(self.device)
        self.model.eval()
        self.threshold = threshold
        self.source_name = source_name

    def detect(
        self,
        image_path: Path,
        image_bgr: np.ndarray,
        text_elements: list[dict[str, Any]],
    ) -> list[Detection]:
        image_rgb = cv2.cvtColor(image_bgr, cv2.COLOR_BGR2RGB)
        inputs = self.processor(images=image_rgb, return_tensors="pt")
        inputs = {k: v.to(self.device) for k, v in inputs.items()}

        with self.torch.no_grad():
            outputs = self.model(**inputs)

        h, w = image_bgr.shape[:2]
        target_sizes = self.torch.tensor([[h, w]], device=self.device)
        results = self.processor.post_process_object_detection(
            outputs,
            threshold=self.threshold,
            target_sizes=target_sizes,
        )
        if not results:
            return []

        detections: list[Detection] = []
        boxes = results[0]["boxes"].detach().cpu().numpy()
        scores = results[0]["scores"].detach().cpu().numpy()
        for idx in range(boxes.shape[0]):
            x1, y1, x2, y2 = [int(round(float(v))) for v in boxes[idx]]
            rect = tlc.Rect(x=x1, y=y1, w=max(0, x2 - x1), h=max(0, y2 - y1)).clipped(w, h)
            if rect is None:
                continue
            if rect.w < 6 or rect.h < 6:
                continue
            detections.append(Detection(rect=rect, score=float(scores[idx]), source=self.source_name))
        return dedupe_detections(detections, iou_threshold=0.95)


class GroundingDinoAdapter(AdapterBase):
    def __init__(
        self,
        *,
        model_id: str,
        source_name: str,
        prompt: str,
        box_threshold: float,
        text_threshold: float,
        device: str,
        allow_download: bool,
    ):
        import torch  # type: ignore
        from transformers import AutoModelForZeroShotObjectDetection, AutoProcessor  # type: ignore

        self.torch = torch
        self.processor = AutoProcessor.from_pretrained(model_id, local_files_only=not allow_download)
        self.model = AutoModelForZeroShotObjectDetection.from_pretrained(
            model_id, local_files_only=not allow_download
        )
        self.device = resolve_torch_device(torch, device)
        self.model.to(self.device)
        self.model.eval()
        self.prompt = prompt
        self.box_threshold = box_threshold
        self.text_threshold = text_threshold
        self.source_name = source_name

    def detect(
        self,
        image_path: Path,
        image_bgr: np.ndarray,
        text_elements: list[dict[str, Any]],
    ) -> list[Detection]:
        image_rgb = cv2.cvtColor(image_bgr, cv2.COLOR_BGR2RGB)
        inputs = self.processor(images=image_rgb, text=self.prompt, return_tensors="pt")
        inputs = {k: v.to(self.device) for k, v in inputs.items()}

        with self.torch.no_grad():
            outputs = self.model(**inputs)

        h, w = image_bgr.shape[:2]
        target_sizes = self.torch.tensor([[h, w]], device=self.device)
        results = self.processor.post_process_grounded_object_detection(
            outputs,
            inputs["input_ids"],
            threshold=self.box_threshold,
            text_threshold=self.text_threshold,
            target_sizes=target_sizes,
        )
        if not results:
            return []

        boxes = results[0].get("boxes")
        scores = results[0].get("scores")
        if boxes is None or scores is None:
            return []
        boxes_np = boxes.detach().cpu().numpy()
        scores_np = scores.detach().cpu().numpy()

        detections: list[Detection] = []
        for idx in range(boxes_np.shape[0]):
            x1, y1, x2, y2 = [int(round(float(v))) for v in boxes_np[idx]]
            rect = tlc.Rect(x=x1, y=y1, w=max(0, x2 - x1), h=max(0, y2 - y1)).clipped(w, h)
            if rect is None:
                continue
            if rect.w < 6 or rect.h < 6:
                continue
            detections.append(Detection(rect=rect, score=float(scores_np[idx]), source=self.source_name))
        return dedupe_detections(detections, iou_threshold=0.95)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Benchmark multiple box detectors on tokenize corpus.")
    parser.add_argument("--input", required=True, help="Input screenshot directory (recursive).")
    parser.add_argument("--output", required=True, help="Output benchmark root directory.")
    parser.add_argument(
        "--models",
        default="cv_edge_ellipse,cv_morph_gradient,rtdetr_r18,table_transformer,grounding_dino",
        help=f"Comma-separated model list. Supported: {', '.join(SUPPORTED_MODELS)}",
    )
    parser.add_argument(
        "--subset",
        default="",
        help=(
            "Optional subset spec: 'manifest:<path-to-txt>' or '<path-to-txt>' or "
            "comma-separated relative image paths."
        ),
    )
    parser.add_argument("--parallel-models", type=int, default=2, help="How many models to run concurrently.")
    parser.add_argument("--write-overlays", action="store_true", help="Write .overlay.png files.")
    parser.add_argument("--include-ocr", action="store_true", help="Include OCR text elements in output payload.")
    parser.add_argument("--include-glyphs", action="store_true", help="Include glyph elements (requires --include-ocr).")
    parser.add_argument("--lang", default="eng", help="Tesseract language for OCR when --include-ocr is set.")
    parser.add_argument("--report-only", action="store_true", help="Only aggregate existing benchmark outputs.")
    parser.add_argument(
        "--device",
        default="auto",
        choices=["auto", "cpu", "mps", "cuda"],
        help="Torch execution device for HF/YOLO adapters.",
    )
    parser.add_argument("--yolo-conf", type=float, default=0.08, help="Confidence threshold for YOLO adapters.")
    parser.add_argument("--hf-threshold", type=float, default=0.2, help="Score threshold for HF object detection.")
    parser.add_argument(
        "--grounding-model",
        default="IDEA-Research/grounding-dino-base",
        help="HF model id for GroundingDINO.",
    )
    parser.add_argument(
        "--grounding-prompt",
        default=DEFAULT_GROUNDING_PROMPT,
        help="GroundingDINO prompt text.",
    )
    parser.add_argument("--grounding-box-threshold", type=float, default=0.2)
    parser.add_argument("--grounding-text-threshold", type=float, default=0.2)
    parser.add_argument(
        "--allow-download",
        action="store_true",
        help="Allow network downloads from Hugging Face (disabled by default).",
    )
    return parser.parse_args()


def resolve_torch_device(torch: Any, requested: str) -> Any:
    if requested == "auto":
        if torch.backends.mps.is_available():
            return torch.device("mps")
        if torch.cuda.is_available():
            return torch.device("cuda")
        return torch.device("cpu")
    return torch.device(requested)


def resolve_yolo_device(requested: str, torch: Any) -> str:
    if requested == "auto":
        if torch.backends.mps.is_available():
            return "mps"
        return "cpu"
    return requested


def parse_models(raw: str) -> list[str]:
    models = [token.strip() for token in raw.split(",") if token.strip()]
    invalid = [m for m in models if m not in SUPPORTED_MODELS]
    if invalid:
        raise ValueError(f"Unsupported model(s): {', '.join(invalid)}")
    deduped: list[str] = []
    for model in models:
        if model not in deduped:
            deduped.append(model)
    return deduped


def iter_images(root: Path) -> list[Path]:
    return tlc.iter_images(root)


def resolve_subset_images(input_root: Path, subset: str, all_images: list[Path]) -> list[Path]:
    if not subset.strip():
        return all_images

    rel_map = {str(path.relative_to(input_root)): path for path in all_images}
    entries: list[str] = []

    if subset.startswith("manifest:"):
        manifest = Path(subset.split(":", 1)[1]).expanduser()
        if not manifest.is_absolute():
            manifest = Path.cwd() / manifest
        lines = manifest.read_text(encoding="utf-8").splitlines()
        entries = [line.strip() for line in lines if line.strip() and not line.strip().startswith("#")]
    else:
        subset_path = Path(subset).expanduser()
        if subset_path.exists():
            lines = subset_path.read_text(encoding="utf-8").splitlines()
            entries = [line.strip() for line in lines if line.strip() and not line.strip().startswith("#")]
        else:
            entries = [token.strip() for token in subset.split(",") if token.strip()]

    images: list[Path] = []
    missing: list[str] = []
    for rel in entries:
        path = rel_map.get(rel)
        if path is None:
            missing.append(rel)
            continue
        images.append(path)
    if missing:
        raise FileNotFoundError(f"Subset contains missing paths under input root: {missing}")
    return images


def build_adapter(model_name: str, args: argparse.Namespace) -> AdapterBase:
    if model_name == "cv_edge_rect":
        return CvAdapter(detector="edge_rect")
    if model_name == "cv_edge_ellipse":
        return CvAdapter(detector="edge_ellipse")
    if model_name == "cv_morph_gradient":
        return CvAdapter(detector="morph_gradient")
    if model_name == "yolov8n":
        with MODEL_INIT_LOCK:
            return YoloAdapter(
                weights="yolov8n.pt",
                source_name="yolo:yolov8n",
                device=args.device,
                conf=args.yolo_conf,
            )
    if model_name == "omniparser_icon":
        from huggingface_hub import hf_hub_download  # type: ignore

        candidates = [
            "icon_detect/model.pt",
            "icon_detect.pt",
            "weights/icon_detect/model.pt",
        ]
        last_error: Exception | None = None
        weights: str | None = None
        with MODEL_INIT_LOCK:
            for candidate in candidates:
                try:
                    weights = hf_hub_download(
                        repo_id="microsoft/OmniParser-v2.0",
                        filename=candidate,
                        local_files_only=not args.allow_download,
                    )
                    break
                except Exception as exc:  # pragma: no cover - depends on remote repo layout
                    last_error = exc
        if weights is None:
            raise RuntimeError(f"failed to resolve OmniParser weights: {last_error}")
        with MODEL_INIT_LOCK:
            return YoloAdapter(
                weights=weights,
                source_name="omniparser:icon_detect",
                device=args.device,
                conf=args.yolo_conf,
            )
    if model_name == "rtdetr_r18":
        with MODEL_INIT_LOCK:
            return HfObjectDetectionAdapter(
                model_id="PekingU/rtdetr_r18vd",
                source_name="hf:rtdetr_r18",
                threshold=args.hf_threshold,
                device=args.device,
                allow_download=args.allow_download,
            )
    if model_name == "table_transformer":
        with MODEL_INIT_LOCK:
            return HfObjectDetectionAdapter(
                model_id="microsoft/table-transformer-detection",
                source_name="hf:table_transformer",
                threshold=args.hf_threshold,
                device=args.device,
                allow_download=args.allow_download,
            )
    if model_name == "grounding_dino":
        with MODEL_INIT_LOCK:
            return GroundingDinoAdapter(
                model_id=args.grounding_model,
                source_name=f"hf:{args.grounding_model}",
                prompt=args.grounding_prompt,
                box_threshold=args.grounding_box_threshold,
                text_threshold=args.grounding_text_threshold,
                device=args.device,
                allow_download=args.allow_download,
            )
    raise ValueError(f"unsupported model: {model_name}")


def dedupe_detections(detections: list[Detection], iou_threshold: float) -> list[Detection]:
    ranked = sorted(detections, key=lambda d: d.score, reverse=True)
    kept: list[Detection] = []
    for candidate in ranked:
        if any(tlc.iou(candidate.rect, existing.rect) > iou_threshold for existing in kept):
            continue
        kept.append(candidate)
    kept.sort(key=lambda d: (d.rect.y, d.rect.x, d.rect.w * d.rect.h))
    return kept


def detections_to_box_elements(detections: list[Detection]) -> list[dict[str, Any]]:
    elements: list[dict[str, Any]] = []
    for idx, detection in enumerate(detections, start=1):
        elements.append(
            {
                "id": f"box_{idx:04d}",
                "type": "box",
                "bbox": detection.rect.as_list(),
                "confidence": round(float(detection.score), 4),
                "source": detection.source,
            }
        )
    return elements


def maybe_build_ocr(
    *,
    image_path: Path,
    image_bgr: np.ndarray,
    include_ocr: bool,
    include_glyphs: bool,
    lang: str,
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    if not include_ocr:
        return [], []
    h, w = image_bgr.shape[:2]
    rows = tlc.run_tesseract_tsv(image_path, lang=lang)
    text_elements = tlc.extract_text_elements(rows, width=w, height=h)
    glyph_elements = tlc.detect_glyphs(image_bgr, text_elements) if include_glyphs else []
    return text_elements, glyph_elements


def run_model(
    *,
    model_name: str,
    args: argparse.Namespace,
    input_root: Path,
    output_root: Path,
    images: list[Path],
) -> dict[str, Any]:
    model_root = output_root / model_name
    model_root.mkdir(parents=True, exist_ok=True)

    started = time.perf_counter()
    try:
        adapter = build_adapter(model_name, args)
    except Exception as exc:
        summary = {
            "model": model_name,
            "status": "failed_to_load",
            "error": repr(exc),
            "images_total": len(images),
            "images_ok": 0,
            "images_failed": len(images),
            "load_seconds": round(time.perf_counter() - started, 4),
            "infer_seconds": 0.0,
            "avg_ms_per_image": None,
        }
        (model_root / "_model_summary.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
        return summary

    load_seconds = time.perf_counter() - started
    infer_start = time.perf_counter()
    images_ok = 0
    images_failed = 0
    boxes_total = 0
    failures: list[dict[str, str]] = []

    for image_path in images:
        rel = image_path.relative_to(input_root)
        out_dir = model_root / rel.parent
        out_dir.mkdir(parents=True, exist_ok=True)
        label_path = out_dir / f"{image_path.stem}.labels.json"
        overlay_path = out_dir / f"{image_path.stem}.overlay.png"

        try:
            image = cv2.imread(str(image_path))
            if image is None:
                raise RuntimeError("failed to decode image")

            text_elements, glyph_elements = maybe_build_ocr(
                image_path=image_path,
                image_bgr=image,
                include_ocr=args.include_ocr,
                include_glyphs=args.include_glyphs,
                lang=args.lang,
            )
            detections = adapter.detect(image_path=image_path, image_bgr=image, text_elements=text_elements)
            box_elements = detections_to_box_elements(detections)
            boxes_total += len(box_elements)

            payload = tlc.make_payload(
                image_path=image_path,
                image=image,
                text_elements=text_elements,
                box_elements=box_elements,
                glyph_elements=glyph_elements,
                box_detector=model_name,
            )
            payload.setdefault("meta", {})["benchmark_model"] = model_name
            payload["meta"]["benchmark_generated_by"] = "tokenize_model_bench"
            label_path.write_text(json.dumps(payload, indent=2), encoding="utf-8")
            if args.write_overlays:
                overlay = tlc.draw_overlay(image, payload)
                cv2.imwrite(str(overlay_path), overlay)
            images_ok += 1
        except Exception as exc:
            images_failed += 1
            if len(failures) < 20:
                failures.append({"image": str(rel), "error": repr(exc)})

    infer_seconds = time.perf_counter() - infer_start
    summary = {
        "model": model_name,
        "status": "ok" if images_failed == 0 else "partial",
        "images_total": len(images),
        "images_ok": images_ok,
        "images_failed": images_failed,
        "boxes_total": boxes_total,
        "boxes_avg_per_ok_image": round(boxes_total / max(1, images_ok), 3),
        "load_seconds": round(load_seconds, 4),
        "infer_seconds": round(infer_seconds, 4),
        "avg_ms_per_image": round((infer_seconds / max(1, images_ok)) * 1000.0, 3),
        "failures": failures,
    }
    (model_root / "_model_summary.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
    return summary


def collect_report(output_root: Path) -> dict[str, Any]:
    model_summaries: list[dict[str, Any]] = []
    for child in sorted(output_root.iterdir()):
        if not child.is_dir():
            continue
        summary_path = child / "_model_summary.json"
        if summary_path.exists():
            try:
                model_summaries.append(json.loads(summary_path.read_text(encoding="utf-8")))
                continue
            except json.JSONDecodeError:
                pass

        labels = sorted(child.rglob("*.labels.json"))
        overlays = sorted(child.rglob("*.overlay.png"))
        model_summaries.append(
            {
                "model": child.name,
                "status": "unknown",
                "images_total": len(labels),
                "images_ok": len(labels),
                "images_failed": 0,
                "boxes_total": None,
                "boxes_avg_per_ok_image": None,
                "load_seconds": None,
                "infer_seconds": None,
                "avg_ms_per_image": None,
                "overlays_total": len(overlays),
                "failures": [],
            }
        )

    return {
        "output_root": str(output_root),
        "models": model_summaries,
        "generated_at_epoch": int(time.time()),
    }


def write_report_files(output_root: Path, report: dict[str, Any]) -> None:
    (output_root / "bench.summary.json").write_text(json.dumps(report, indent=2), encoding="utf-8")

    lines = [
        "model\tstatus\timages_total\timages_ok\timages_failed\tboxes_total\tboxes_avg_per_ok_image\tavg_ms_per_image"
    ]
    for item in report.get("models", []):
        lines.append(
            "\t".join(
                [
                    str(item.get("model", "")),
                    str(item.get("status", "")),
                    str(item.get("images_total", "")),
                    str(item.get("images_ok", "")),
                    str(item.get("images_failed", "")),
                    str(item.get("boxes_total", "")),
                    str(item.get("boxes_avg_per_ok_image", "")),
                    str(item.get("avg_ms_per_image", "")),
                ]
            )
        )
    (output_root / "bench.summary.tsv").write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> int:
    args = parse_args()
    output_root = Path(args.output).expanduser().resolve()
    output_root.mkdir(parents=True, exist_ok=True)

    if args.report_only:
        report = collect_report(output_root)
        write_report_files(output_root, report)
        print(json.dumps(report, indent=2))
        return 0

    input_root = Path(args.input).expanduser().resolve()
    all_images = iter_images(input_root)
    if not all_images:
        raise SystemExit(f"No images found under: {input_root}")

    models = parse_models(args.models)
    images = resolve_subset_images(input_root, args.subset, all_images)
    if not images:
        raise SystemExit("Subset resolved to zero images")

    max_workers = max(1, min(args.parallel_models, len(models)))
    summaries: list[dict[str, Any]] = []

    if max_workers == 1:
        for model_name in models:
            summaries.append(
                run_model(
                    model_name=model_name,
                    args=args,
                    input_root=input_root,
                    output_root=output_root,
                    images=images,
                )
            )
    else:
        with concurrent.futures.ThreadPoolExecutor(max_workers=max_workers) as executor:
            future_map = {
                executor.submit(
                    run_model,
                    model_name=model_name,
                    args=args,
                    input_root=input_root,
                    output_root=output_root,
                    images=images,
                ): model_name
                for model_name in models
            }
            for future in concurrent.futures.as_completed(future_map):
                summaries.append(future.result())

    summaries.sort(key=lambda item: models.index(item.get("model", "")))
    report = {
        "input_root": str(input_root),
        "output_root": str(output_root),
        "models_requested": models,
        "images_total": len(images),
        "parallel_models": max_workers,
        "include_ocr": bool(args.include_ocr),
        "include_glyphs": bool(args.include_glyphs),
        "models": summaries,
    }
    write_report_files(output_root, report)
    print(json.dumps(report, indent=2))
    return 0


if __name__ == "__main__":
    os.environ.setdefault("TOKENIZERS_PARALLELISM", "false")
    raise SystemExit(main())
