#!/usr/bin/env python3
"""Generate `.window.json` test data from Settings screenshots using OpenCV.

Expected output JSON shape:
{
  "image": "dark-forest-center.png",
  "window": { "x": 791, "y": 114, "width": 718, "height": 628 },
  "occluded": false
}

Typical usage:
  python3 src/desktop/scripts/dev/generate_settings_window_data.py \
    src/desktop/daemon/tests/fixtures/settings-screenshots \
    --write-overlays \
    --write-crops
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

try:
    import cv2  # type: ignore
    import numpy as np
except ImportError as exc:  # pragma: no cover - runtime dependency check
    missing = "opencv-python numpy"
    raise SystemExit(
        "Missing Python dependencies. Install them with:\n"
        f"  python3 -m pip install {missing}\n"
        f"Import error: {exc}"
    )


IMAGE_SUFFIXES = {".png", ".jpg", ".jpeg"}


@dataclass(frozen=True)
class Bounds:
    x: int
    y: int
    width: int
    height: int

    @property
    def x2(self) -> int:
        return self.x + self.width

    @property
    def y2(self) -> int:
        return self.y + self.height

    def as_json(self) -> dict[str, int]:
        return {
            "x": self.x,
            "y": self.y,
            "width": self.width,
            "height": self.height,
        }


@dataclass
class DetectorContext:
    processor: Any = None
    model: Any = None
    torch: Any = None
    pil_image: Any = None


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Detect application window bounds in screenshots and write .window.json files."
    )
    parser.add_argument(
        "inputs",
        nargs="+",
        help="One or more image files or directories containing screenshots.",
    )
    parser.add_argument(
        "--write-overlays",
        action="store_true",
        help="Write <image>.window.overlay.png review images.",
    )
    parser.add_argument(
        "--output-dir",
        help="Directory for generated JSON/crops/overlays. Defaults to <input dir>/out for directory inputs, or <image dir>/out for file inputs.",
    )
    parser.add_argument(
        "--detector",
        choices=("cv", "grounding-dino", "hybrid"),
        default="hybrid",
        help="Detection strategy. 'hybrid' uses GroundingDINO when available, then CV refinement/fallback.",
    )
    parser.add_argument(
        "--grounding-model",
        default="IDEA-Research/grounding-dino-base",
        help="Hugging Face model id for GroundingDINO. Default: IDEA-Research/grounding-dino-base.",
    )
    parser.add_argument(
        "--device",
        choices=("auto", "cpu", "mps", "cuda"),
        default="cpu",
        help="Execution device for GroundingDINO. Default: cpu.",
    )
    parser.add_argument(
        "--prompt",
        default="application window . dialog . panel . system settings window .",
        help="Grounding prompt used when --detector includes grounding-dino.",
    )
    parser.add_argument(
        "--box-threshold",
        type=float,
        default=0.25,
        help="GroundingDINO box threshold. Default: 0.25.",
    )
    parser.add_argument(
        "--text-threshold",
        type=float,
        default=0.2,
        help="GroundingDINO text threshold. Default: 0.2.",
    )
    parser.add_argument(
        "--write-crops",
        action="store_true",
        help="Write <image>.crop.png crops based on detected bounds.",
    )
    parser.add_argument(
        "--write-sips-crops",
        action="store_true",
        help="Write <image>.sips.crop.png using macOS sips for crop verification.",
    )
    parser.add_argument(
        "--overwrite",
        action="store_true",
        help="Overwrite existing .window.json files.",
    )
    parser.add_argument(
        "--min-width",
        type=int,
        default=320,
        help="Ignore detections narrower than this. Default: 320.",
    )
    parser.add_argument(
        "--min-height",
        type=int,
        default=240,
        help="Ignore detections shorter than this. Default: 240.",
    )
    parser.add_argument(
        "--max-border-gap",
        type=int,
        default=20,
        help="Morphology kernel size used to join fragmented borders. Default: 20.",
    )
    parser.add_argument(
        "--stdout",
        action="store_true",
        help="Print generated JSON to stdout.",
    )
    return parser.parse_args()


def iter_images(inputs: list[str]) -> list[Path]:
    images: list[Path] = []
    for raw in inputs:
        path = Path(raw)
        if path.is_dir():
            for child in sorted(path.iterdir()):
                if child.suffix.lower() in IMAGE_SUFFIXES and ".crop." not in child.name:
                    images.append(child)
            continue
        if path.suffix.lower() in IMAGE_SUFFIXES:
            images.append(path)
    return images


def resolve_output_dir(args: argparse.Namespace, source_path: Path) -> Path:
    if args.output_dir:
        output_dir = Path(args.output_dir)
    else:
        output_dir = source_path.parent / "out"
    output_dir.mkdir(parents=True, exist_ok=True)
    return output_dir


def output_path_for(output_dir: Path, image_path: Path, suffix: str) -> Path:
    return output_dir / f"{image_path.stem}{suffix}"


def load_grounding_detector(args: argparse.Namespace) -> DetectorContext:
    try:
        import torch  # type: ignore
        from PIL import Image  # type: ignore
        from transformers import AutoModelForZeroShotObjectDetection, AutoProcessor  # type: ignore
    except ImportError as exc:
        raise RuntimeError(
            "GroundingDINO dependencies are not installed. Run `uv sync --extra grounding`."
        ) from exc

    device = args.device
    if device == "auto":
        device = "cpu"
        if torch.backends.mps.is_available():
            device = "mps"
        elif torch.cuda.is_available():
            device = "cuda"

    dtype = torch.float16 if device != "cpu" else torch.float32
    processor = AutoProcessor.from_pretrained(args.grounding_model)
    model = AutoModelForZeroShotObjectDetection.from_pretrained(
        args.grounding_model,
        dtype=dtype,
    )
    model.to(device)
    model.eval()
    return DetectorContext(processor=processor, model=model, torch=torch, pil_image=Image)


def detect_window_bounds(
    image: np.ndarray,
    args: argparse.Namespace,
    detector: DetectorContext | None = None,
    *,
    min_width: int,
    min_height: int,
    max_border_gap: int,
) -> Bounds | None:
    if args.detector in {"grounding-dino", "hybrid"}:
        grounding_bounds = detect_window_bounds_with_grounding(
            image,
            args=args,
            detector=detector,
            min_width=min_width,
            min_height=min_height,
            max_border_gap=max_border_gap,
        )
        if grounding_bounds is not None:
            return grounding_bounds
        if args.detector == "grounding-dino":
            return None

    return detect_window_bounds_with_cv(
        image,
        min_width=min_width,
        min_height=min_height,
        max_border_gap=max_border_gap,
    )


def detect_window_bounds_with_cv(
    image: np.ndarray,
    *,
    min_width: int,
    min_height: int,
    max_border_gap: int,
) -> Bounds | None:
    traffic_lights = find_traffic_light_centers(image)
    bounds = detect_bounds_from_traffic_lights(
        image,
        traffic_lights,
        min_width=min_width,
        min_height=min_height,
    )
    if bounds is not None:
        return bounds

    gray = cv2.cvtColor(image, cv2.COLOR_BGR2GRAY)
    gray[:100, :] = 0
    blurred = cv2.GaussianBlur(gray, (5, 5), 0)

    edges = cv2.Canny(blurred, 40, 140)
    kernel = cv2.getStructuringElement(
        cv2.MORPH_RECT, (max(3, max_border_gap), max(3, max_border_gap))
    )
    closed = cv2.morphologyEx(edges, cv2.MORPH_CLOSE, kernel, iterations=2)

    contours, _ = cv2.findContours(closed, cv2.RETR_LIST, cv2.CHAIN_APPROX_SIMPLE)
    if not contours:
        return None

    image_h, image_w = gray.shape
    image_area = float(image_w * image_h)
    center_x = image_w / 2.0
    center_y = image_h / 2.0

    best_score = -1.0
    best_bounds: Bounds | None = None

    for contour in contours:
        x, y, w, h = cv2.boundingRect(contour)
        if w < min_width or h < min_height:
            continue

        area = float(w * h)
        if area < image_area * 0.05:
            continue
        if area > image_area * 0.72:
            continue

        aspect = w / float(h)
        if aspect < 0.75 or aspect > 2.2:
            continue

        touches_left = x <= 2
        touches_top = y <= 102
        touches_right = x + w >= image_w - 2
        touches_bottom = y + h >= image_h - 2
        edge_touches = sum(
            [touches_left, touches_top, touches_right, touches_bottom]
        )
        if edge_touches >= 2:
            continue

        x2 = min(image_w, x + w)
        y2 = min(image_h, y + h)
        roi = gray[y:y2, x:x2]
        if roi.size == 0:
            continue

        roi_edges = edges[y:y2, x:x2]
        perimeter = max(1.0, 2.0 * (w + h))
        border_strength = cv2.countNonZero(roi_edges) / perimeter
        fill_ratio = area / image_area
        center_penalty = (
            abs((x + w / 2.0) - center_x) / image_w
            + abs((y + h / 2.0) - center_y) / image_h
        )

        title_band_h = max(12, min(48, h // 10))
        title_band = gray[y : min(image_h, y + title_band_h), x:x2]
        title_variance = float(np.var(title_band)) if title_band.size else 0.0
        traffic_light_bonus = 0.0
        for tl_x, tl_y in traffic_lights:
            within_x = x + 8 <= tl_x <= x + min(120, w // 4)
            within_y = y + 6 <= tl_y <= y + min(56, h // 8)
            if within_x and within_y:
                traffic_light_bonus = 3.0
                break
        if traffic_lights and traffic_light_bonus == 0.0:
            continue

        score = (
            fill_ratio * 4.0
            + border_strength * 2.0
            + min(title_variance / 2000.0, 1.0)
            + traffic_light_bonus
            - center_penalty
        )

        bounds = Bounds(x=x, y=y, width=w, height=h)
        if score > best_score:
            best_score = score
            best_bounds = bounds

    return best_bounds


def find_traffic_light_centers(image: np.ndarray) -> list[tuple[int, int]]:
    hsv = cv2.cvtColor(image, cv2.COLOR_BGR2HSV)

    masks = {
        "red": cv2.bitwise_or(
            cv2.inRange(hsv, np.array([0, 90, 90]), np.array([12, 255, 255])),
            cv2.inRange(hsv, np.array([170, 90, 90]), np.array([180, 255, 255])),
        ),
        "yellow": cv2.inRange(hsv, np.array([18, 80, 80]), np.array([42, 255, 255])),
        "green": cv2.inRange(hsv, np.array([45, 70, 70]), np.array([95, 255, 255])),
    }

    components = {
        color: connected_component_centers(mask) for color, mask in masks.items()
    }

    centers: list[tuple[int, int]] = []
    for red_x, red_y in components["red"]:
        for yellow_x, yellow_y in components["yellow"]:
            if yellow_x <= red_x:
                continue
            if abs(yellow_y - red_y) > 8:
                continue
            if not 10 <= (yellow_x - red_x) <= 34:
                continue
            for green_x, green_y in components["green"]:
                if green_x <= yellow_x:
                    continue
                if abs(green_y - red_y) > 8:
                    continue
                if not 10 <= (green_x - yellow_x) <= 34:
                    continue
                if red_y < 100:
                    continue
                centers.append((red_x, red_y))
                break

    return centers


def connected_component_centers(mask: np.ndarray) -> list[tuple[int, int]]:
    count, labels, stats, centroids = cv2.connectedComponentsWithStats(mask, connectivity=8)
    centers: list[tuple[int, int]] = []
    for label in range(1, count):
        x, y, w, h, area = stats[label]
        if area < 8 or area > 400:
            continue
        if w < 3 or h < 3 or w > 24 or h > 24:
            continue
        cx, cy = centroids[label]
        centers.append((int(round(cx)), int(round(cy))))
    return centers


def detect_bounds_from_traffic_lights(
    image: np.ndarray,
    traffic_lights: list[tuple[int, int]],
    *,
    min_width: int,
    min_height: int,
) -> Bounds | None:
    if not traffic_lights:
        return None

    image_h, image_w = image.shape[:2]
    blurred = cv2.GaussianBlur(image, (7, 7), 0)
    best_bounds: Bounds | None = None
    best_area = -1

    for red_x, red_y in traffic_lights:
        union_mask = np.zeros((image_h, image_w), dtype=np.uint8)
        for dx, dy in ((24, 16), (72, 40), (40, 110), (210, 110)):
            seed_x = min(max(red_x + dx, 0), image_w - 1)
            seed_y = min(max(red_y + dy, 100), image_h - 1)
            mask = np.zeros((image_h + 2, image_w + 2), dtype=np.uint8)
            flood_source = blurred.copy()
            cv2.floodFill(
                flood_source,
                mask,
                (seed_x, seed_y),
                (255, 255, 255),
                loDiff=(28, 28, 28),
                upDiff=(28, 28, 28),
                flags=cv2.FLOODFILL_FIXED_RANGE,
            )
            union_mask = cv2.bitwise_or(union_mask, mask[1:-1, 1:-1])

        kernel = cv2.getStructuringElement(cv2.MORPH_RECT, (15, 15))
        union_mask = cv2.morphologyEx(union_mask, cv2.MORPH_CLOSE, kernel, iterations=2)
        points = cv2.findNonZero(union_mask)
        if points is None:
            continue

        x, y, w, h = cv2.boundingRect(points)
        if w < min_width or h < min_height:
            continue
        if w * h > image_w * image_h * 0.72:
            continue

        touches = sum([x <= 2, y <= 102, x + w >= image_w - 2, y + h >= image_h - 2])
        if touches >= 2:
            continue

        area = w * h
        if area > best_area:
            best_area = area
            best_bounds = Bounds(x=x, y=y, width=w, height=h)

    return best_bounds


def detect_window_bounds_with_grounding(
    image: np.ndarray,
    *,
    args: argparse.Namespace,
    detector: DetectorContext | None,
    min_width: int,
    min_height: int,
    max_border_gap: int,
) -> Bounds | None:
    if detector is None or detector.model is None or detector.processor is None:
        return None

    pil_image = detector.pil_image.fromarray(cv2.cvtColor(image, cv2.COLOR_BGR2RGB))
    inputs = detector.processor(images=pil_image, text=args.prompt, return_tensors="pt")
    device = next(detector.model.parameters()).device
    inputs = {key: value.to(device) for key, value in inputs.items()}

    with detector.torch.no_grad():
        outputs = detector.model(**inputs)

    target_sizes = detector.torch.tensor([pil_image.size[::-1]], device=device)
    results = detector.processor.post_process_grounded_object_detection(
        outputs,
        inputs["input_ids"],
        threshold=args.box_threshold,
        text_threshold=args.text_threshold,
        target_sizes=target_sizes,
    )
    if not results:
        return None

    image_h, image_w = image.shape[:2]
    traffic_lights = find_traffic_light_centers(image)
    best_score = -1.0
    best_bounds: Bounds | None = None

    labels = results[0].get("text_labels") or results[0].get("labels") or []
    for box, score, label in zip(
        results[0]["boxes"],
        results[0]["scores"],
        labels,
        strict=False,
    ):
        candidate = bounds_from_xyxy(box.tolist(), image_w, image_h)
        if candidate is None:
            continue
        if candidate.width < min_width or candidate.height < min_height:
            continue
        if touches_many_edges(candidate, image_w, image_h):
            continue

        refined = refine_bounds_near_candidate(
            image,
            candidate,
            min_width=min_width,
            min_height=min_height,
            max_border_gap=max_border_gap,
        ) or candidate

        candidate_score = float(score)
        if contains_any_traffic_light(refined, traffic_lights):
            candidate_score += 1.5
        candidate_score += bounds_area(refined) / float(image_w * image_h)
        if "window" in str(label):
            candidate_score += 0.5

        if candidate_score > best_score:
            best_score = candidate_score
            best_bounds = refined

    return best_bounds


def refine_bounds_near_candidate(
    image: np.ndarray,
    candidate: Bounds,
    *,
    min_width: int,
    min_height: int,
    max_border_gap: int,
) -> Bounds | None:
    image_h, image_w = image.shape[:2]
    pad_x = max(32, candidate.width // 7)
    pad_y = max(24, candidate.height // 7)
    roi_x0 = max(0, candidate.x - pad_x)
    roi_y0 = max(100, candidate.y - pad_y)
    roi_x1 = min(image_w, candidate.x2 + pad_x)
    roi_y1 = min(image_h, candidate.y2 + pad_y)
    if roi_x1 - roi_x0 < min_width or roi_y1 - roi_y0 < min_height:
        return candidate

    roi = image[roi_y0:roi_y1, roi_x0:roi_x1]
    gray = cv2.cvtColor(roi, cv2.COLOR_BGR2GRAY)
    blurred = cv2.GaussianBlur(gray, (5, 5), 0)
    edges = cv2.Canny(blurred, 40, 140)
    kernel = cv2.getStructuringElement(
        cv2.MORPH_RECT, (max(3, max_border_gap // 2), max(3, max_border_gap // 2))
    )
    closed = cv2.morphologyEx(edges, cv2.MORPH_CLOSE, kernel, iterations=2)
    contours, _ = cv2.findContours(closed, cv2.RETR_LIST, cv2.CHAIN_APPROX_SIMPLE)
    if not contours:
        return candidate

    target = Bounds(
        x=candidate.x - roi_x0,
        y=candidate.y - roi_y0,
        width=candidate.width,
        height=candidate.height,
    )
    roi_area = float(gray.shape[0] * gray.shape[1])
    best_score = -1.0
    best_bounds: Bounds | None = None

    for contour in contours:
        x, y, w, h = cv2.boundingRect(contour)
        bounds = Bounds(x=x, y=y, width=w, height=h)
        if bounds.width < min_width or bounds.height < min_height:
            continue
        if bounds_area(bounds) > roi_area * 0.95:
            continue

        score = intersection_over_union(bounds, target) * 4.0
        score += intersection_over_union(bounds, expand_bounds(target, 24, gray.shape[1], gray.shape[0])) * 1.5
        if contains_any_traffic_light(
            Bounds(
                x=bounds.x + roi_x0,
                y=bounds.y + roi_y0,
                width=bounds.width,
                height=bounds.height,
            ),
            find_traffic_light_centers(image),
        ):
            score += 1.5
        if score > best_score:
            best_score = score
            best_bounds = bounds

    if best_bounds is None:
        return candidate

    return Bounds(
        x=best_bounds.x + roi_x0,
        y=best_bounds.y + roi_y0,
        width=best_bounds.width,
        height=best_bounds.height,
    )


def bounds_from_xyxy(box: list[float], image_w: int, image_h: int) -> Bounds | None:
    if len(box) != 4:
        return None
    x0 = max(0, min(image_w - 1, int(round(box[0]))))
    y0 = max(0, min(image_h - 1, int(round(box[1]))))
    x1 = max(0, min(image_w, int(round(box[2]))))
    y1 = max(0, min(image_h, int(round(box[3]))))
    if x1 <= x0 or y1 <= y0:
        return None
    return Bounds(x=x0, y=y0, width=x1 - x0, height=y1 - y0)


def expand_bounds(bounds: Bounds, pad: int, image_w: int, image_h: int) -> Bounds:
    x0 = max(0, bounds.x - pad)
    y0 = max(0, bounds.y - pad)
    x1 = min(image_w, bounds.x2 + pad)
    y1 = min(image_h, bounds.y2 + pad)
    return Bounds(x=x0, y=y0, width=x1 - x0, height=y1 - y0)


def bounds_area(bounds: Bounds) -> int:
    return max(0, bounds.width) * max(0, bounds.height)


def touches_many_edges(bounds: Bounds, image_w: int, image_h: int) -> bool:
    touches = sum(
        [
            bounds.x <= 2,
            bounds.y <= 102,
            bounds.x2 >= image_w - 2,
            bounds.y2 >= image_h - 2,
        ]
    )
    return touches >= 2


def contains_any_traffic_light(bounds: Bounds, traffic_lights: list[tuple[int, int]]) -> bool:
    for tl_x, tl_y in traffic_lights:
        if bounds.x <= tl_x <= bounds.x2 and bounds.y <= tl_y <= bounds.y + min(64, bounds.height // 6):
            return True
    return False


def intersection_over_union(a: Bounds, b: Bounds) -> float:
    x0 = max(a.x, b.x)
    y0 = max(a.y, b.y)
    x1 = min(a.x2, b.x2)
    y1 = min(a.y2, b.y2)
    inter_w = max(0, x1 - x0)
    inter_h = max(0, y1 - y0)
    intersection = inter_w * inter_h
    union = bounds_area(a) + bounds_area(b) - intersection
    if union <= 0:
        return 0.0
    return intersection / float(union)


def write_json(output_dir: Path, image_path: Path, bounds: Bounds, overwrite: bool) -> Path | None:
    output_path = output_path_for(output_dir, image_path, ".window.json")
    if output_path.exists() and not overwrite:
        return None

    payload = {
        "image": image_path.name,
        "window": bounds.as_json(),
        "occluded": False,
    }
    output_path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    return output_path


def write_overlay(output_dir: Path, image_path: Path, image: np.ndarray, bounds: Bounds) -> Path:
    overlay = image.copy()
    cv2.rectangle(
        overlay,
        (bounds.x, bounds.y),
        (bounds.x2, bounds.y2),
        (64, 220, 110),
        3,
    )
    output_path = output_path_for(output_dir, image_path, ".window.overlay.png")
    cv2.imwrite(str(output_path), overlay)
    return output_path


def write_crop(output_dir: Path, image_path: Path, image: np.ndarray, bounds: Bounds) -> Path:
    crop = image[bounds.y : bounds.y2, bounds.x : bounds.x2]
    output_path = output_path_for(output_dir, image_path, ".crop.png")
    cv2.imwrite(str(output_path), crop)
    return output_path


def write_sips_crop(output_dir: Path, image_path: Path, bounds: Bounds) -> Path:
    output_path = output_path_for(output_dir, image_path, ".sips.crop.png")
    command = [
        "sips",
        "-c",
        str(bounds.height),
        str(bounds.width),
        "--cropOffset",
        str(bounds.y),
        str(bounds.x),
        str(image_path),
        "--out",
        str(output_path),
    ]
    subprocess.run(command, check=True, capture_output=True, text=True)
    return output_path


def process_image(
    path: Path,
    args: argparse.Namespace,
    detector: DetectorContext | None,
) -> int:
    image = cv2.imread(str(path))
    if image is None:
        print(f"skip: failed to read {path}", file=sys.stderr)
        return 1

    bounds = detect_window_bounds(
        image,
        args,
        detector,
        min_width=args.min_width,
        min_height=args.min_height,
        max_border_gap=args.max_border_gap,
    )
    if bounds is None:
        print(f"skip: no window detected for {path}", file=sys.stderr)
        return 1

    payload = {
        "image": path.name,
        "window": bounds.as_json(),
        "occluded": False,
    }

    output_dir = resolve_output_dir(args, path)
    written = write_json(output_dir, path, bounds, overwrite=args.overwrite)
    if args.write_overlays:
        write_overlay(output_dir, path, image, bounds)
    if args.write_crops:
        write_crop(output_dir, path, image, bounds)
    if args.write_sips_crops:
        write_sips_crop(output_dir, path, bounds)
    if args.stdout or written is not None:
        print(json.dumps(payload))
    elif written is None:
        print(f"keep: {output_path_for(output_dir, path, '.window.json').name}", file=sys.stderr)
    return 0


def main() -> int:
    args = parse_args()
    images = iter_images(args.inputs)
    if not images:
        print("No input images found.", file=sys.stderr)
        return 1

    detector: DetectorContext | None = None
    if args.detector in {"grounding-dino", "hybrid"}:
        try:
            detector = load_grounding_detector(args)
        except RuntimeError as exc:
            if args.detector == "grounding-dino":
                print(str(exc), file=sys.stderr)
                return 1
            print(f"warn: {exc} Falling back to CV detector.", file=sys.stderr)

    failures = 0
    for path in images:
        failures += process_image(path, args, detector)
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
