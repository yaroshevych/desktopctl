#[path = "../src/vision/regions.rs"]
mod regions;

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use image::ImageReader;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct WindowLabel {
    image: String,
    window: LabeledBounds,
    #[serde(default)]
    occluded: bool,
}

#[derive(Debug, Deserialize)]
struct LabeledBounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/settings-screenshots")
}

fn fixture_stems_with_ext(dir: &Path, ext: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(dir).expect("read fixtures dir") {
        let path = entry.expect("read entry").path();
        if !path.is_file() {
            continue;
        }
        let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or_default();
        if file_name.ends_with(ext) {
            names.insert(file_name.trim_end_matches(ext).to_string());
        }
    }
    names
}

fn assert_bounds_close(
    actual: &desktop_core::protocol::Bounds,
    expected: &LabeledBounds,
    tol: f64,
    label: &str,
) -> Option<String> {
    let dx = (actual.x - expected.x).abs();
    let dy = (actual.y - expected.y).abs();
    let dw = (actual.width - expected.width).abs();
    let dh = (actual.height - expected.height).abs();
    if dx > tol || dy > tol || dw > tol || dh > tol {
        return Some(format!(
            "{label}: expected=({:.1},{:.1},{:.1},{:.1}) actual=({:.1},{:.1},{:.1},{:.1}) deltas=({dx:.1},{dy:.1},{dw:.1},{dh:.1}) tol={tol:.1}",
            expected.x,
            expected.y,
            expected.width,
            expected.height,
            actual.x,
            actual.y,
            actual.width,
            actual.height
        ));
    }
    None
}

#[test]
fn settings_screenshot_label_files_match_png_fixtures() {
    let dir = fixtures_dir();
    let png_stems = fixture_stems_with_ext(&dir, ".png");
    let label_stems = fixture_stems_with_ext(&dir, ".window.json");
    assert!(
        !png_stems.is_empty(),
        "no .png fixtures found in {}",
        dir.display()
    );
    assert_eq!(
        png_stems, label_stems,
        "png/label mismatch in {}",
        dir.display()
    );
}

#[test]
fn traffic_light_candidates_include_labeled_window_top_left() {
    let dir = fixtures_dir();
    let mut failures = Vec::new();

    let mut label_paths = fs::read_dir(&dir)
        .expect("read fixtures dir")
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let file_name = path.file_name()?.to_str()?;
            if file_name.ends_with(".window.json") {
                Some(path)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    label_paths.sort();

    for label_path in label_paths {
        let raw = fs::read_to_string(&label_path).expect("read label file");
        let label: WindowLabel = serde_json::from_str(&raw).expect("parse label JSON");
        let image_path = dir.join(&label.image);
        let image = ImageReader::open(&image_path)
            .expect("open fixture image")
            .decode()
            .expect("decode fixture image")
            .to_rgba8();

        let candidates = regions::traffic_light_candidates_for_test(&image);
        if candidates.is_empty() {
            failures.push(format!("{}: traffic lights not detected", label.image));
            continue;
        }

        let mut matched = false;
        for (tl_x, tl_y) in &candidates {
            let dx = *tl_x as f64 - label.window.x;
            let dy = *tl_y as f64 - label.window.y;
            // Red traffic-light center is expected near the top-left corner of the window.
            let x_ok = (18.0..=42.0).contains(&dx);
            let y_ok = (18.0..=40.0).contains(&dy);
            if x_ok && y_ok {
                matched = true;
                break;
            }
        }
        if !matched {
            let rendered = candidates
                .iter()
                .map(|(x, y)| format!("({x},{y})"))
                .collect::<Vec<_>>()
                .join(", ");
            failures.push(format!(
                "{}: no traffic-light candidate near window top-left, window=({:.1},{:.1}), candidates=[{}]",
                label.image, label.window.x, label.window.y, rendered
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "traffic-light anchor mismatches:\n{}",
        failures.join("\n")
    );
}

#[test]
#[ignore = "strict benchmark test: run explicitly while improving detector accuracy"]
fn detects_window_bounds_on_labeled_settings_screenshots() {
    let dir = fixtures_dir();
    let mut failures = Vec::new();

    let mut label_paths = fs::read_dir(&dir)
        .expect("read fixtures dir")
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let file_name = path.file_name()?.to_str()?;
            if file_name.ends_with(".window.json") {
                Some(path)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    label_paths.sort();

    for label_path in label_paths {
        let raw = fs::read_to_string(&label_path).expect("read label file");
        let label: WindowLabel = serde_json::from_str(&raw).expect("parse label JSON");
        let image_path = dir.join(&label.image);
        let image = ImageReader::open(&image_path)
            .expect("open fixture image")
            .decode()
            .expect("decode fixture image")
            .to_rgba8();
        let detected = regions::detect_settings_regions(&image);
        let Some(window) = detected.window_bounds.as_ref() else {
            failures.push(format!(
                "{}: expected window bounds from label, detector returned null",
                label.image
            ));
            continue;
        };

        let tol = if label.occluded { 40.0 } else { 24.0 };
        if let Some(msg) = assert_bounds_close(window, &label.window, tol, &label.image) {
            failures.push(msg);
        }
    }

    assert!(
        failures.is_empty(),
        "settings-screenshot window mismatches:\n{}",
        failures.join("\n")
    );
}
