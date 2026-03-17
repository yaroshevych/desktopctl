#[path = "../src/vision/tokenize_boxes.rs"]
mod tokenize_boxes;

use std::{
    fs,
    path::{Path, PathBuf},
};

use desktop_core::protocol::Bounds;
use image::ImageReader;
use serde::Deserialize;

const DEFAULT_LABELS_ROOT: &str = "/Users/oleg/Projects/DesktopCtl/tmp/tokenize-20260317-phase1/labels/selected/grounding_dino/broad_020_020_full52/grounding_dino";
const IOU_MATCH_THRESHOLD: f64 = 0.5;
const EXPECTED_RECALL_THRESHOLD: f64 = 0.35;
const EXPECTED_PRECISION_THRESHOLD: f64 = 0.03;

#[derive(Debug, Deserialize)]
struct LabelFile {
    image: LabeledImageMeta,
    windows: Vec<LabeledWindow>,
}

#[derive(Debug, Deserialize)]
struct LabeledImageMeta {
    path: String,
}

#[derive(Debug, Deserialize)]
struct LabeledWindow {
    elements: Vec<LabeledElement>,
}

#[derive(Debug, Deserialize)]
struct LabeledElement {
    #[serde(rename = "type")]
    kind: String,
    bbox: [f64; 4],
}

fn labels_root() -> PathBuf {
    std::env::var("DESKTOPCTL_TOKENIZE_LABELS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_LABELS_ROOT))
}

fn collect_label_files(root: &Path) -> Vec<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.file_name().and_then(|s| s.to_str()).unwrap_or_default().ends_with(".labels.json") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn iou(a: &Bounds, b: &Bounds) -> f64 {
    let ax2 = a.x + a.width;
    let ay2 = a.y + a.height;
    let bx2 = b.x + b.width;
    let by2 = b.y + b.height;
    let ix1 = a.x.max(b.x);
    let iy1 = a.y.max(b.y);
    let ix2 = ax2.min(bx2);
    let iy2 = ay2.min(by2);
    let iw = (ix2 - ix1).max(0.0);
    let ih = (iy2 - iy1).max(0.0);
    let inter = iw * ih;
    if inter <= 0.0 {
        return 0.0;
    }
    let union = (a.width * a.height) + (b.width * b.height) - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
}

fn to_bounds(raw: [f64; 4]) -> Option<Bounds> {
    let width = raw[2].max(0.0);
    let height = raw[3].max(0.0);
    if width < 2.0 || height < 2.0 {
        return None;
    }
    Some(Bounds {
        x: raw[0],
        y: raw[1],
        width,
        height,
    })
}

#[test]
fn broad_grounding_labels_have_minimum_box_recall() {
    let root = labels_root();
    assert!(
        root.exists(),
        "labels root does not exist: {} (set DESKTOPCTL_TOKENIZE_LABELS_DIR if needed)",
        root.display()
    );

    let label_files = collect_label_files(&root);
    assert!(
        !label_files.is_empty(),
        "no .labels.json files found under {}",
        root.display()
    );

    let mut total_expected = 0usize;
    let mut total_matched = 0usize;
    let mut total_predicted = 0usize;
    let mut total_predicted_matched = 0usize;

    for label_path in label_files {
        let raw = fs::read_to_string(&label_path).expect("read label file");
        let labels: LabelFile = serde_json::from_str(&raw).expect("parse label JSON");
        let image_path = PathBuf::from(&labels.image.path);
        let image = ImageReader::open(&image_path)
            .expect("open labeled image")
            .decode()
            .expect("decode labeled image")
            .to_rgba8();

        let predicted = tokenize_boxes::detect_ui_boxes(&image);
        let expected: Vec<Bounds> = labels
            .windows
            .iter()
            .flat_map(|window| window.elements.iter())
            .filter(|element| element.kind == "box")
            .filter_map(|element| to_bounds(element.bbox))
            .collect();
        assert!(
            !expected.is_empty(),
            "no expected box labels in {}",
            label_path.display()
        );

        total_expected += expected.len();
        total_predicted += predicted.len();
        for e in &expected {
            if predicted
                .iter()
                .any(|p| iou(p, e) >= IOU_MATCH_THRESHOLD)
            {
                total_matched += 1;
            }
        }

        for p in &predicted {
            if expected.iter().any(|e| iou(p, e) >= IOU_MATCH_THRESHOLD) {
                total_predicted_matched += 1;
            }
        }
    }

    let recall = if total_expected == 0 {
        0.0
    } else {
        total_matched as f64 / total_expected as f64
    };
    let precision = if total_predicted == 0 {
        0.0
    } else {
        total_predicted_matched as f64 / total_predicted as f64
    };
    println!(
        "tokenize_box_labels metrics: recall={:.3} precision={:.3} matched={} expected={} predicted={} predicted_matched={}",
        recall, precision, total_matched, total_expected, total_predicted, total_predicted_matched
    );
    assert!(
        recall >= EXPECTED_RECALL_THRESHOLD,
        "box recall too low: {:.3} (matched {} / expected {}), threshold {:.3}",
        recall,
        total_matched,
        total_expected,
        EXPECTED_RECALL_THRESHOLD
    );
    assert!(
        precision >= EXPECTED_PRECISION_THRESHOLD,
        "box precision too low: {:.3} (predicted-matched {} / predicted {}), threshold {:.3}",
        precision,
        total_predicted_matched,
        total_predicted,
        EXPECTED_PRECISION_THRESHOLD
    );
}
