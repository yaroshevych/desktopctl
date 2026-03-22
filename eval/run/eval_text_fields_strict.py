#!/usr/bin/env python3
"""Strict evaluator for text-field detection.

Loads GT labels and tokenizer predictions from a run folder, computes
IoU@0.50 recall/precision, boundary error stats, clutter counts, and
writes failure overlays.

Usage:
    uv run run/eval_text_fields_strict.py --run-dir datasets/runs/<run_id>
    uv run run/eval_text_fields_strict.py --run-dir datasets/runs/<run_id> --iou 0.35
"""

from __future__ import annotations

import argparse
import csv
import json
import sys
from pathlib import Path

try:
    from PIL import Image, ImageDraw, ImageFont
    _HAS_PIL = True
except ImportError:
    _HAS_PIL = False


# ── geometry ──────────────────────────────────────────────────────────────────

def iou(a: list[float], b: list[float]) -> float:
    """Compute IoU of two [x, y, w, h] boxes."""
    ax1, ay1, aw, ah = a
    bx1, by1, bw, bh = b
    ax2, ay2 = ax1 + aw, ay1 + ah
    bx2, by2 = bx1 + bw, by1 + bh
    ix1 = max(ax1, bx1)
    iy1 = max(ay1, by1)
    ix2 = min(ax2, bx2)
    iy2 = min(ay2, by2)
    if ix2 <= ix1 or iy2 <= iy1:
        return 0.0
    inter = (ix2 - ix1) * (iy2 - iy1)
    union = aw * ah + bw * bh - inter
    return inter / union if union > 0 else 0.0


def best_iou(gt: list[float], preds: list[list[float]]) -> tuple[float, int]:
    """Return (best_iou, best_pred_idx) for a GT box against all predictions."""
    best, best_i = 0.0, -1
    for i, p in enumerate(preds):
        v = iou(gt, p)
        if v > best:
            best, best_i = v, i
    return best, best_i


def containing_count(gt: list[float], preds: list[list[float]], threshold: float) -> int:
    """Count predictions with IoU >= threshold for a given GT box."""
    return sum(1 for p in preds if iou(gt, p) >= threshold)


# ── data loading ──────────────────────────────────────────────────────────────

def load_boxes(label_path: Path, type_filter: str = "box") -> list[list[float]]:
    """Load all elements of given type from a label.json windows list."""
    data = json.loads(label_path.read_text())
    boxes = []
    for win in data.get("windows", []):
        for el in win.get("elements", []):
            if el.get("type") == type_filter:
                boxes.append(el["bbox"])
    return boxes


# ── per-sample evaluation ─────────────────────────────────────────────────────

def eval_sample(
    label_id: str,
    gt_label_path: Path,
    pred_label_path: Path,
    iou_thresh: float,
) -> dict:
    gt_boxes = load_boxes(gt_label_path)
    pred_boxes = load_boxes(pred_label_path)

    matched_gt = set()
    matched_pred = set()
    gt_details = []

    for gi, gt in enumerate(gt_boxes):
        best, best_i = best_iou(gt, pred_boxes)
        hit = best >= iou_thresh
        if hit:
            matched_gt.add(gi)
            matched_pred.add(best_i)
        # Boundary deltas (signed, pred - gt): left, right, top, bottom
        if best_i >= 0:
            p = pred_boxes[best_i]
            dl = p[0] - gt[0]
            dr = (p[0] + p[2]) - (gt[0] + gt[2])
            dt = p[1] - gt[1]
            db = (p[1] + p[3]) - (gt[1] + gt[3])
        else:
            dl = dr = dt = db = None
        n_containing = containing_count(gt, pred_boxes, iou_thresh)
        gt_details.append({
            "gt_idx": gi,
            "gt_bbox": gt,
            "best_iou": best,
            "best_pred_idx": best_i,
            "best_pred_bbox": pred_boxes[best_i] if best_i >= 0 else None,
            "hit": hit,
            "delta_left": dl,
            "delta_right": dr,
            "delta_top": dt,
            "delta_bottom": db,
            "n_containing": n_containing,
        })

    n_gt = len(gt_boxes)
    n_pred = len(pred_boxes)
    n_matched_gt = len(matched_gt)
    n_matched_pred = len(matched_pred)
    recall = n_matched_gt / n_gt if n_gt > 0 else 1.0
    precision = n_matched_pred / n_pred if n_pred > 0 else 1.0

    return {
        "label_id": label_id,
        "n_gt": n_gt,
        "n_pred": n_pred,
        "n_matched_gt": n_matched_gt,
        "n_matched_pred": n_matched_pred,
        "recall": recall,
        "precision": precision,
        "gt_details": gt_details,
    }


# ── overlay rendering ─────────────────────────────────────────────────────────

def render_failure_overlay(
    image_path: Path,
    gt_boxes: list[list[float]],
    pred_boxes: list[list[float]],
    iou_thresh: float,
    out_path: Path,
) -> None:
    if not _HAS_PIL:
        return
    img = Image.open(image_path).convert("RGBA")
    overlay = Image.new("RGBA", img.size, (0, 0, 0, 0))
    draw = ImageDraw.Draw(overlay)

    # Draw all predictions in blue (semi-transparent)
    for p in pred_boxes:
        x, y, w, h = p
        draw.rectangle([x, y, x + w, y + h], outline=(100, 100, 255, 180), width=2)

    # Draw GT boxes: green=hit, red=miss
    for gt in gt_boxes:
        best, _ = best_iou(gt, pred_boxes)
        hit = best >= iou_thresh
        color = (0, 220, 60, 220) if hit else (255, 40, 40, 220)
        x, y, w, h = gt
        draw.rectangle([x, y, x + w, y + h], outline=color, width=3)
        # Label IoU in corner
        draw.text((x + 2, y + 2), f"{best:.2f}", fill=color)

    composite = Image.alpha_composite(img, overlay).convert("RGB")
    out_path.parent.mkdir(parents=True, exist_ok=True)
    composite.save(out_path)


# ── stats helpers ─────────────────────────────────────────────────────────────

def percentile(values: list[float], p: float) -> float:
    if not values:
        return float("nan")
    values = sorted(values)
    idx = (len(values) - 1) * p / 100
    lo = int(idx)
    hi = min(lo + 1, len(values) - 1)
    return values[lo] + (values[hi] - values[lo]) * (idx - lo)


def delta_stats(samples: list[dict], field: str) -> dict:
    vals = [d[field] for s in samples for d in s["gt_details"] if d[field] is not None]
    if not vals:
        return {}
    return {
        "mean": sum(vals) / len(vals),
        "p50": percentile(vals, 50),
        "p90": percentile(vals, 90),
        "min": min(vals),
        "max": max(vals),
    }


def containing_stats(samples: list[dict]) -> dict:
    vals = [d["n_containing"] for s in samples for d in s["gt_details"]]
    if not vals:
        return {}
    return {
        "median": percentile(vals, 50),
        "p90": percentile(vals, 90),
        "mean": sum(vals) / len(vals),
    }


# ── main ──────────────────────────────────────────────────────────────────────

def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--run-dir", type=Path, required=True,
                   help="Run folder containing artifacts.csv and results/")
    p.add_argument("--iou", type=float, default=0.50,
                   help="IoU threshold for hit (default: 0.50)")
    p.add_argument("--out-dir", type=Path,
                   help="Output directory for metrics/failures (default: <run-dir>/eval_strict)")
    p.add_argument("--no-overlays", action="store_true",
                   help="Skip rendering failure overlays")
    return p.parse_args()


def main() -> None:
    args = parse_args()
    run_dir = args.run_dir.resolve()
    out_dir = args.out_dir or (run_dir / "eval_strict")
    out_dir.mkdir(parents=True, exist_ok=True)
    iou_thresh = args.iou

    artifacts_csv = run_dir / "artifacts.csv"
    if not artifacts_csv.exists():
        sys.exit(f"artifacts.csv not found in {run_dir}")

    results_dir = run_dir / "results"
    samples = []
    failures = []

    with artifacts_csv.open() as f:
        reader = csv.DictReader(f)
        rows = list(reader)

    print(f"Evaluating {len(rows)} samples  IoU@{iou_thresh:.2f}  run={run_dir.name}")

    for row in rows:
        label_id = row["label_id"]
        gt_label_path = Path(row["label_path"])
        pred_label_path = results_dir / label_id / "label.json"

        if not gt_label_path.exists():
            print(f"  WARN: GT label missing: {gt_label_path}")
            continue
        if not pred_label_path.exists():
            print(f"  WARN: pred label missing: {pred_label_path}")
            continue

        result = eval_sample(label_id, gt_label_path, pred_label_path, iou_thresh)
        samples.append(result)

        if result["recall"] < 1.0:
            failures.append(result)

        # Quick per-sample line
        status = "OK" if result["recall"] == 1.0 else "FAIL"
        print(
            f"  [{status}] {label_id}  "
            f"recall={result['recall']:.3f} ({result['n_matched_gt']}/{result['n_gt']})  "
            f"prec={result['precision']:.3f}  pred={result['n_pred']}"
        )

    if not samples:
        sys.exit("No samples evaluated.")

    total_gt = sum(s["n_gt"] for s in samples)
    total_matched = sum(s["n_matched_gt"] for s in samples)
    total_pred = sum(s["n_pred"] for s in samples)
    total_matched_pred = sum(s["n_matched_pred"] for s in samples)
    global_recall = total_matched / total_gt if total_gt > 0 else 1.0
    global_precision = total_matched_pred / total_pred if total_pred > 0 else 1.0

    print()
    print(f"=== Global ===")
    print(f"  recall    {global_recall:.4f}  ({total_matched}/{total_gt} GT matched)")
    print(f"  precision {global_precision:.4f}  ({total_matched_pred}/{total_pred} pred matched)")
    print(f"  failures  {len(failures)}/{len(samples)} samples with recall < 1.0")

    # Boundary delta stats (over matched GT only)
    matched_samples = [s for s in samples if s["n_matched_gt"] > 0]
    print()
    print("=== Boundary deltas (matched GT, signed pred-gt) ===")
    for field, label in [
        ("delta_left", "left "),
        ("delta_right", "right"),
        ("delta_top", "top  "),
        ("delta_bottom", "bot  "),
    ]:
        st = delta_stats(matched_samples, field)
        if st:
            print(f"  {label}  mean={st['mean']:+.1f}  p50={st['p50']:+.1f}  "
                  f"p90={st['p90']:+.1f}  [{st['min']:+.1f}, {st['max']:+.1f}]")

    cs = containing_stats(samples)
    print()
    print(f"=== Clutter (# pred with IoU>={iou_thresh:.2f} per GT) ===")
    print(f"  median={cs.get('median', 0):.1f}  p90={cs.get('p90', 0):.1f}  "
          f"mean={cs.get('mean', 0):.2f}")

    # Write metrics.json
    metrics = {
        "run_id": run_dir.name,
        "iou_threshold": iou_thresh,
        "n_samples": len(samples),
        "n_failures": len(failures),
        "global_recall": global_recall,
        "global_precision": global_precision,
        "total_gt": total_gt,
        "total_matched_gt": total_matched,
        "total_pred": total_pred,
        "total_matched_pred": total_matched_pred,
        "boundary_deltas": {
            f: delta_stats(matched_samples, f)
            for f in ["delta_left", "delta_right", "delta_top", "delta_bottom"]
        },
        "clutter": cs,
        "per_sample": [
            {k: v for k, v in s.items() if k != "gt_details"}
            for s in samples
        ],
    }
    (out_dir / "metrics.json").write_text(json.dumps(metrics, indent=2))
    print(f"\n  metrics -> {out_dir / 'metrics.json'}")

    # Write failures.csv
    fail_csv = out_dir / "failures.csv"
    with fail_csv.open("w", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=[
            "label_id", "n_gt", "n_pred", "n_matched_gt", "recall", "precision",
        ])
        writer.writeheader()
        for s in sorted(failures, key=lambda x: x["recall"]):
            writer.writerow({k: s[k] for k in writer.fieldnames})
    print(f"  failures -> {fail_csv}")

    # Render overlays
    if not args.no_overlays and _HAS_PIL:
        overlays_dir = out_dir / "overlays"
        overlays_dir.mkdir(exist_ok=True)
        # Render all samples (both hits and failures), sorted by recall asc
        for s in sorted(samples, key=lambda x: x["recall"]):
            label_id = s["label_id"]
            # Find image path from artifacts
            row = next((r for r in rows if r["label_id"] == label_id), None)
            if row is None:
                continue
            image_path = Path(row["image_path"])
            if not image_path.exists():
                continue
            gt_boxes = [d["gt_bbox"] for d in s["gt_details"]]
            pred_boxes = load_boxes(results_dir / label_id / "label.json")
            out_png = overlays_dir / f"{label_id}.png"
            render_failure_overlay(image_path, gt_boxes, pred_boxes, iou_thresh, out_png)
        print(f"  overlays -> {overlays_dir}")
    elif not _HAS_PIL:
        print("  overlays skipped (Pillow not available)")


if __name__ == "__main__":
    main()
