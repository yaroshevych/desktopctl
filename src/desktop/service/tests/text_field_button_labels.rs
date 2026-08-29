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
use text_group::{TextBox, group_words_into_lines, split_wide_textbox, tighten_to_content};
use tokenize_boxes::DetectedControl;

/// Per-dimension pixel tolerance for bbox matching.
const BBOX_TOLERANCE: f64 = 20.0;

/// Minimum recall to pass (fraction of expected controls matched), ignoring kind.
const MIN_CONTROL_RECALL: f64 = 1.0;

/// Maximum allowed false positives across all images, ignoring kind.
const MAX_CONTROL_FP: usize = 14;

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
    #[serde(default)]
    not_controls: Vec<String>,
}

#[derive(Debug, Clone)]
struct ExpectedControl {
    bounds: Bounds,
    text: String,
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

fn closest_distance(predicted: &[DetectedControl], expected: &Bounds) -> String {
    if predicted.is_empty() {
        return "no predictions".to_string();
    }
    let (best, dist) = predicted
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

/// 1:1 greedy matching: for each expected label, find the closest unmatched
/// prediction within tolerance. Returns (matched_count, false_positive_count).
fn match_controls(
    predictions: &[DetectedControl],
    labels: &[ExpectedControl],
    file: &str,
) -> (usize, usize) {
    let mut used = vec![false; predictions.len()];
    let mut matched = 0usize;

    for label in labels {
        let expected = &label.bounds;
        // Find closest unused prediction within tolerance.
        let best = predictions
            .iter()
            .enumerate()
            .filter(|(i, _)| !used[*i])
            .filter(|(_, c)| bbox_matches(&c.bounds, &expected))
            .min_by(|(_, a), (_, b)| {
                let da = bbox_distance(&a.bounds, &expected);
                let db = bbox_distance(&b.bounds, &expected);
                da.partial_cmp(&db).unwrap()
            });

        if let Some((i, _)) = best {
            used[i] = true;
            matched += 1;
        } else {
            let hint = closest_distance(predictions, expected);
            eprintln!(
                "  MISS control in {}: expected [{:.0},{:.0},{:.0},{:.0}] {:?}  {}",
                file, expected.x, expected.y, expected.width, expected.height, label.text, hint,
            );
        }
    }

    let fp = used.iter().filter(|u| !**u).count();
    (matched, fp)
}

fn bbox_overlaps(a: &Bounds, b: &Bounds) -> bool {
    a.x < b.x + b.width && a.x + a.width > b.x && a.y < b.y + b.height && a.y + a.height > b.y
}

fn bbox_distance(a: &Bounds, b: &Bounds) -> f64 {
    (a.x - b.x).abs() + (a.y - b.y).abs() + (a.width - b.width).abs() + (a.height - b.height).abs()
}

fn enforce_dictionary_control_uniqueness(
    entry: &ImageControls,
    text_lines: &[TextLine],
    predicted: &[DetectedControl],
) {
    let expected = entry.buttons.iter().find(|b| b.text == "Dictionary");
    let Some(expected) = expected else {
        return;
    };
    let expected = to_bounds(&expected.bbox);

    // Any OCR line containing "Dictionary" is considered dictionary-related.
    let dict_lines: Vec<&TextLine> = text_lines
        .iter()
        .filter(|l| l.text.contains("Dictionary"))
        .collect();
    if dict_lines.is_empty() {
        return;
    }

    let overlaps_dict_line = |c: &DetectedControl| {
        dict_lines
            .iter()
            .any(|l| bbox_overlaps(&c.bounds, &l.bounds))
    };

    // Consume the expected Dictionary button first.
    let consumed_idx = predicted
        .iter()
        .enumerate()
        .filter(|(_, c)| overlaps_dict_line(c))
        .find(|(_, c)| bbox_matches(&c.bounds, &expected))
        .map(|(i, _)| i);

    assert!(
        consumed_idx.is_some(),
        "Expected Dictionary button not found in {} at [{:.0},{:.0},{:.0},{:.0}]",
        entry.file,
        expected.x,
        expected.y,
        expected.width,
        expected.height
    );

    // After consuming the expected one, there must be no additional
    // Dictionary-related controls.
    for (i, c) in predicted.iter().enumerate() {
        if Some(i) == consumed_idx {
            continue;
        }
        if overlaps_dict_line(c) {
            panic!(
                "Dictionary control conflict in {}: extra control at [{:.0},{:.0},{:.0},{:.0}]",
                entry.file, c.bounds.x, c.bounds.y, c.bounds.width, c.bounds.height
            );
        }
    }
}

struct TextLine {
    bounds: Bounds,
    text: String,
}

fn run_text_pipeline(
    image_path: &std::path::Path,
) -> Result<(Vec<TextLine>, metal_pipeline::ProcessedFrame), String> {
    let image = ImageReader::open(image_path)
        .unwrap_or_else(|e| panic!("open image {}: {}", image_path.display(), e))
        .decode()
        .unwrap_or_else(|e| panic!("decode image {}: {}", image_path.display(), e))
        .to_rgba8();

    let texts = ocr::recognize_text(&image)
        .map_err(|e| format!("ocr failed for {}: {}", image_path.display(), e.message))?;
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
    let text_lines: Vec<TextLine> = lines
        .iter()
        .map(|l| TextLine {
            bounds: l.bounds.clone(),
            text: l.text.clone(),
        })
        .collect();
    Ok((text_lines, frame))
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

    let mut control_total = 0usize;
    let mut control_matched = 0usize;
    let mut control_fp = 0usize;

    for entry in &fixtures {
        let image_path = dir.join(&entry.file);
        if !image_path.exists() {
            eprintln!("SKIP: fixture image not found: {}", entry.file);
            continue;
        }

        let (text_lines, frame) = match run_text_pipeline(&image_path) {
            Ok(result) => result,
            Err(err) => {
                eprintln!("SKIP: {}", err);
                continue;
            }
        };
        let line_bounds: Vec<Bounds> = text_lines.iter().map(|l| l.bounds.clone()).collect();
        let predicted = tokenize_boxes::detect_controls(&frame, &line_bounds);

        let expected_controls: Vec<ExpectedControl> = entry
            .text_fields
            .iter()
            .chain(entry.buttons.iter())
            .map(|label| ExpectedControl {
                bounds: to_bounds(&label.bbox),
                text: label.text.clone(),
            })
            .collect();

        // 1:1 greedy matching: each expected consumes at most one prediction.
        let (matched, fp) = match_controls(&predicted, &expected_controls, &entry.file);
        control_total += expected_controls.len();
        control_matched += matched;
        control_fp += fp;

        // Negative assertions: text lines matching these strings must NOT
        // produce a detected control.
        for neg_text in &entry.not_controls {
            // "Dictionary" is handled by a dedicated consume-and-uniqueness
            // rule below (one expected button allowed, no extras).
            if neg_text == "Dictionary" {
                continue;
            }
            // Find the text line containing this string.
            let neg_line = text_lines
                .iter()
                .find(|l| l.text.contains(neg_text.as_str()));
            if let Some(line) = neg_line {
                let bad = predicted
                    .iter()
                    .any(|c| bbox_overlaps(&c.bounds, &line.bounds));
                assert!(
                    !bad,
                    "NEGATIVE: {:?} in {} was detected as a control but should not be",
                    neg_text, entry.file,
                );
            }
        }

        // Dictionary-specific guard:
        // consume the expected "Dictionary" button, then ensure no additional
        // Dictionary-related control (button/text_field) exists.
        enforce_dictionary_control_uniqueness(entry, &text_lines, &predicted);
    }

    let control_recall = if control_total == 0 {
        1.0
    } else {
        control_matched as f64 / control_total as f64
    };

    println!(
        "controls: recall={:.3} ({}/{}) fp={}",
        control_recall, control_matched, control_total, control_fp,
    );

    assert!(
        control_recall >= MIN_CONTROL_RECALL,
        "control recall too low: {:.3} (threshold {:.3})",
        control_recall,
        MIN_CONTROL_RECALL,
    );
    assert!(
        control_fp <= MAX_CONTROL_FP,
        "control false positives too high: {} (max {})",
        control_fp,
        MAX_CONTROL_FP,
    );
}
