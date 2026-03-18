#[path = "../src/vision/tokenize_boxes.rs"]
#[allow(dead_code)]
mod tokenize_boxes;

use std::{
    fs,
    path::{Path, PathBuf},
};

use desktop_core::protocol::Bounds;
use image::ImageReader;
use serde::Deserialize;

const IOU_MATCH_THRESHOLD: f64 = 0.45;

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
    id: String,
    #[serde(rename = "type")]
    kind: String,
    bbox: [f64; 4],
}

fn labels_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tokenize_manual/notes")
}

fn collect_label_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return files;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .ends_with(".labels.json")
        {
            files.push(path);
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
fn manual_notes_label_fixtures_are_present_and_well_formed() {
    let root = labels_root();
    assert!(
        root.exists(),
        "missing manual labels root: {}",
        root.display()
    );

    let files = collect_label_files(&root);
    assert!(
        files.len() >= 2,
        "expected at least two manual notes label files"
    );

    for label_path in files {
        let raw = fs::read_to_string(&label_path).expect("read manual label file");
        let labels: LabelFile = serde_json::from_str(&raw).expect("parse manual label JSON");
        let image_path = if Path::new(&labels.image.path).is_absolute() {
            PathBuf::from(&labels.image.path)
        } else {
            label_path
                .parent()
                .expect("label parent")
                .join(&labels.image.path)
        };
        assert!(
            image_path.exists(),
            "missing fixture image: {}",
            image_path.display()
        );

        let expected_boxes: Vec<&LabeledElement> = labels
            .windows
            .iter()
            .flat_map(|window| window.elements.iter())
            .filter(|element| element.kind == "box")
            .collect();
        let expected_texts: Vec<&LabeledElement> = labels
            .windows
            .iter()
            .flat_map(|window| window.elements.iter())
            .filter(|element| element.kind == "text")
            .collect();
        assert!(
            expected_boxes.len() >= 7,
            "expected >=7 manual boxes in {}",
            label_path.display()
        );
        assert!(
            expected_texts.len() >= 5,
            "expected >=5 manual text seeds in {}",
            label_path.display()
        );
        assert!(
            expected_boxes
                .iter()
                .any(|element| element.id.contains("sidebar_container")),
            "expected sidebar container label in {}",
            label_path.display()
        );
        assert!(
            expected_boxes
                .iter()
                .any(|element| element.id.contains("list_row_selected")),
            "expected selected list row label in {}",
            label_path.display()
        );
    }
}

#[test]
fn notes_manual_labels_have_sidebar_and_list_coverage() {
    let root = labels_root();
    let files = collect_label_files(&root);
    assert!(!files.is_empty(), "no manual labels in {}", root.display());

    let mut sidebar_total = 0usize;
    let mut sidebar_matched = 0usize;
    let mut list_total = 0usize;
    let mut list_matched = 0usize;
    let mut overall_total = 0usize;
    let mut overall_matched = 0usize;

    for label_path in files {
        let raw = fs::read_to_string(&label_path).expect("read manual label file");
        let labels: LabelFile = serde_json::from_str(&raw).expect("parse manual label JSON");
        let image_path = if Path::new(&labels.image.path).is_absolute() {
            PathBuf::from(&labels.image.path)
        } else {
            label_path
                .parent()
                .expect("label parent")
                .join(&labels.image.path)
        };

        let image = ImageReader::open(&image_path)
            .expect("open fixture image")
            .decode()
            .expect("decode fixture image")
            .to_rgba8();
        let text_bounds: Vec<Bounds> = labels
            .windows
            .iter()
            .flat_map(|window| window.elements.iter())
            .filter(|element| element.kind == "text")
            .filter_map(|element| to_bounds(element.bbox))
            .collect();
        assert!(
            !text_bounds.is_empty(),
            "expected text seeds in {}",
            label_path.display()
        );
        let predicted = tokenize_boxes::detect_ui_boxes_with_text(&image, &text_bounds);

        let expected: Vec<&LabeledElement> = labels
            .windows
            .iter()
            .flat_map(|window| window.elements.iter())
            .filter(|element| element.kind == "box")
            .collect();

        for element in expected {
            let Some(expected_bounds) = to_bounds(element.bbox) else {
                continue;
            };
            let matched = predicted
                .iter()
                .any(|candidate| iou(candidate, &expected_bounds) >= IOU_MATCH_THRESHOLD);
            overall_total += 1;
            if matched {
                overall_matched += 1;
            }
            if element.id.contains("sidebar_") {
                sidebar_total += 1;
                if matched {
                    sidebar_matched += 1;
                }
            }
            if element.id.contains("list_") {
                list_total += 1;
                if matched {
                    list_matched += 1;
                }
            }
        }
    }

    let overall_recall = if overall_total == 0 {
        0.0
    } else {
        overall_matched as f64 / overall_total as f64
    };
    let sidebar_recall = if sidebar_total == 0 {
        0.0
    } else {
        sidebar_matched as f64 / sidebar_total as f64
    };
    let list_recall = if list_total == 0 {
        0.0
    } else {
        list_matched as f64 / list_total as f64
    };

    println!(
        "manual_notes_coverage overall={:.3} sidebar={:.3} list={:.3} matched={{overall:{}/{} sidebar:{}/{} list:{}/{}}}",
        overall_recall,
        sidebar_recall,
        list_recall,
        overall_matched,
        overall_total,
        sidebar_matched,
        sidebar_total,
        list_matched,
        list_total
    );

    assert!(
        overall_recall >= 0.90,
        "overall notes recall too low: {:.3}",
        overall_recall
    );
    assert!(
        sidebar_recall >= 0.90,
        "sidebar notes recall too low: {:.3}",
        sidebar_recall
    );
    assert!(
        list_recall >= 0.90,
        "list notes recall too low: {:.3}",
        list_recall
    );
}
