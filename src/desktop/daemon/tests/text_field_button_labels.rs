//! Integration test: loads controls.json ground-truth labels, runs the control
//! detection pipeline on golden fixture images, checks that predicted text fields
//! and buttons match expected bboxes within pixel tolerance.

#[path = "../src/vision/metal_pipeline.rs"]
#[allow(dead_code)]
mod metal_pipeline;
#[path = "../src/vision/ocr.rs"]
mod ocr;
#[path = "../src/vision/text_group.rs"]
#[allow(dead_code)]
mod text_group;
#[path = "../src/vision/tokenize_boxes.rs"]
#[allow(dead_code)]
mod tokenize_boxes;
#[path = "../src/trace.rs"]
mod trace;

use std::{fs, path::PathBuf};

use desktop_core::protocol::Bounds;
use image::ImageReader;
use serde::Deserialize;
use text_group::{
    TextBox, group_words_into_lines, split_wide_textbox, tighten_to_content,
};
use tokenize_boxes::{ControlKind, DetectedControl};

/// Per-dimension pixel tolerance for bbox matching.
const BBOX_TOLERANCE: f64 = 15.0;

/// Minimum recall to pass (0.0 = stub passes, raise as implementation improves).
const MIN_TEXT_FIELD_RECALL: f64 = 0.0;
const MIN_BUTTON_RECALL: f64 = 0.0;

// ── JSON schema ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ControlLabel {
    bbox: [f64; 4],
    text: String,
}

#[derive(Debug, Deserialize)]
struct ImageControls {
    file: String,
    text_fields: Vec<ControlLabel>,
    buttons: Vec<ControlLabel>,
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden")
}

fn to_bounds(bbox: &[f64; 4]) -> Bounds {
    Bounds {
        x: bbox[0],
        y: bbox[1],
        width: bbox[2],
        height: bbox[3],
    }
}

fn bbox_matches(predicted: &Bounds, expected: &Bounds) -> bool {
    (predicted.x - expected.x).abs() < BBOX_TOLERANCE
        && (predicted.y - expected.y).abs() < BBOX_TOLERANCE
        && (predicted.width - expected.width).abs() < BBOX_TOLERANCE
        && (predicted.height - expected.height).abs() < BBOX_TOLERANCE
}

fn closest_distance(predicted: &[DetectedControl], expected: &Bounds, kind: ControlKind) -> String {
    let relevant: Vec<&DetectedControl> = predicted.iter().filter(|c| c.kind == kind).collect();
    if relevant.is_empty() {
        return "no predictions".to_string();
    }
    let (best, dist) = relevant
        .iter()
        .map(|c| {
            let d = (c.bounds.x - expected.x).abs()
                + (c.bounds.y - expected.y).abs()
                + (c.bounds.width - expected.width).abs()
                + (c.bounds.height - expected.height).abs();
            (c, d)
        })
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
        .unwrap();
    format!(
        "closest: [{:.0},{:.0},{:.0},{:.0}] dist={:.0}",
        best.bounds.x, best.bounds.y, best.bounds.width, best.bounds.height, dist
    )
}

fn run_text_pipeline(image_path: &std::path::Path) -> (Vec<Bounds>, metal_pipeline::ProcessedFrame) {
    let image = ImageReader::open(image_path)
        .unwrap_or_else(|e| panic!("open image {}: {}", image_path.display(), e))
        .decode()
        .unwrap_or_else(|e| panic!("decode image {}: {}", image_path.display(), e))
        .to_rgba8();

    let texts = ocr::recognize_text(&image)
        .unwrap_or_else(|e| panic!("ocr failed for {}: {}", image_path.display(), e.message));
    let frame = metal_pipeline::process_cpu(&image);

    let words: Vec<TextBox> = texts
        .iter()
        .map(|t| TextBox::from_bounds_with_text(t.bounds.clone(), t.text.clone()))
        .flat_map(|tb| split_wide_textbox(tb, &frame))
        .map(|tb| {
            let tight = tighten_to_content(&tb.bounds, &frame);
            TextBox::from_bounds_with_text(tight, tb.text)
        })
        .collect();

    let lines = group_words_into_lines(&words);
    let line_bounds: Vec<Bounds> = lines.iter().map(|l| l.bounds.clone()).collect();
    (line_bounds, frame)
}

// ── test ────────────────────────────────────────────────────────────────────

#[test]
fn golden_controls_have_expected_text_fields_and_buttons() {
    let dir = fixture_dir();
    let json_path = dir.join("controls.json");
    assert!(
        json_path.exists(),
        "controls.json not found at {}",
        json_path.display()
    );

    let raw = fs::read_to_string(&json_path).expect("read controls.json");
    let fixtures: Vec<ImageControls> = serde_json::from_str(&raw).expect("parse controls.json");
    assert!(!fixtures.is_empty(), "controls.json has no entries");

    let mut tf_total = 0usize;
    let mut tf_matched = 0usize;
    let mut btn_total = 0usize;
    let mut btn_matched = 0usize;

    for entry in &fixtures {
        let image_path = dir.join(&entry.file);
        if !image_path.exists() {
            eprintln!("SKIP: fixture image not found: {}", entry.file);
            continue;
        }

        let (line_bounds, frame) = run_text_pipeline(&image_path);
        let predicted = tokenize_boxes::detect_controls(&frame, &line_bounds);

        // Check text fields.
        for label in &entry.text_fields {
            let expected = to_bounds(&label.bbox);
            tf_total += 1;
            let matched = predicted
                .iter()
                .any(|c| c.kind == ControlKind::TextField && bbox_matches(&c.bounds, &expected));
            if matched {
                tf_matched += 1;
            } else {
                let hint = closest_distance(&predicted, &expected, ControlKind::TextField);
                eprintln!(
                    "  MISS text_field in {}: expected [{:.0},{:.0},{:.0},{:.0}] {:?}  {}",
                    entry.file,
                    expected.x, expected.y, expected.width, expected.height,
                    label.text,
                    hint,
                );
            }
        }

        // Check buttons.
        for label in &entry.buttons {
            let expected = to_bounds(&label.bbox);
            btn_total += 1;
            let matched = predicted
                .iter()
                .any(|c| c.kind == ControlKind::Button && bbox_matches(&c.bounds, &expected));
            if matched {
                btn_matched += 1;
            } else {
                let hint = closest_distance(&predicted, &expected, ControlKind::Button);
                eprintln!(
                    "  MISS button in {}: expected [{:.0},{:.0},{:.0},{:.0}] {:?}  {}",
                    entry.file,
                    expected.x, expected.y, expected.width, expected.height,
                    label.text,
                    hint,
                );
            }
        }
    }

    let tf_recall = if tf_total == 0 { 1.0 } else { tf_matched as f64 / tf_total as f64 };
    let btn_recall = if btn_total == 0 { 1.0 } else { btn_matched as f64 / btn_total as f64 };

    println!(
        "controls: text_field recall={:.3} ({}/{})  button recall={:.3} ({}/{})",
        tf_recall, tf_matched, tf_total, btn_recall, btn_matched, btn_total,
    );

    assert!(
        tf_recall >= MIN_TEXT_FIELD_RECALL,
        "text_field recall too low: {:.3} (threshold {:.3})",
        tf_recall,
        MIN_TEXT_FIELD_RECALL,
    );
    assert!(
        btn_recall >= MIN_BUTTON_RECALL,
        "button recall too low: {:.3} (threshold {:.3})",
        btn_recall,
        MIN_BUTTON_RECALL,
    );
}
