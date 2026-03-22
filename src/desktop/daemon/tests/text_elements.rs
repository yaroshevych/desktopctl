#[path = "../src/vision/metal_pipeline.rs"]
#[allow(dead_code)]
mod metal_pipeline;
#[path = "../src/vision/text_group.rs"]
#[allow(dead_code)]
mod text_group;
#[path = "../src/vision/tokenize_boxes.rs"]
#[allow(dead_code)]
mod tokenize_boxes;
#[path = "../src/vision/ocr.rs"]
mod ocr;
#[path = "../src/trace.rs"]
mod trace;

use std::path::{Path, PathBuf};

use image::ImageReader;
use text_group::{
    TextBox, group_lines_into_paragraphs, group_words_into_lines, split_wide_textbox,
    tighten_to_content,
};
use std::collections::HashMap;

#[derive(Debug)]
struct TextAnalysis {
    final_fields: Vec<String>,
}

#[derive(Debug)]
struct ImageExpectation {
    file: &'static str,
    expected_final_fields: &'static [&'static str],
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn fixture_path(file: &str) -> PathBuf {
    repo_root().join("src/desktop/daemon/tests/fixtures/golden").join(file)
}

fn normalize(s: &str) -> String {
    s.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn area(tb: &TextBox) -> f64 {
    (tb.bounds.width.max(0.0)) * (tb.bounds.height.max(0.0))
}

fn overlap_area(a: &TextBox, b: &TextBox) -> f64 {
    let ax2 = a.bounds.x + a.bounds.width;
    let ay2 = a.bounds.y + a.bounds.height;
    let bx2 = b.bounds.x + b.bounds.width;
    let by2 = b.bounds.y + b.bounds.height;
    let ix1 = a.bounds.x.max(b.bounds.x);
    let iy1 = a.bounds.y.max(b.bounds.y);
    let ix2 = ax2.min(bx2);
    let iy2 = ay2.min(by2);
    let iw = (ix2 - ix1).max(0.0);
    let ih = (iy2 - iy1).max(0.0);
    iw * ih
}

fn analyze_image(image_path: &Path) -> TextAnalysis {
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
    let paragraphs = group_lines_into_paragraphs(&lines);
    let mut final_fields: Vec<String> = paragraphs.iter().map(|t| normalize(&t.text)).collect();

    for line in &lines {
        let line_area = area(line).max(1.0);
        let grouped = paragraphs
            .iter()
            .any(|p| overlap_area(line, p) / line_area >= 0.85);
        if !grouped {
            final_fields.push(normalize(&line.text));
        }
    }

    TextAnalysis { final_fields }
}

fn multiset(items: &[String]) -> HashMap<String, usize> {
    let mut m = HashMap::new();
    for item in items {
        *m.entry(item.clone()).or_insert(0) += 1;
    }
    m
}

fn assert_image_expectation(expectation: &ImageExpectation) {
    let path = fixture_path(expectation.file);
    assert!(path.exists(), "fixture not found: {}", path.display());

    let analysis = analyze_image(&path);

    let expected: Vec<String> = expectation
        .expected_final_fields
        .iter()
        .map(|s| normalize(s))
        .collect();
    let expected_ms = multiset(&expected);
    let actual_ms = multiset(&analysis.final_fields);
    assert_eq!(
        actual_ms, expected_ms,
        "final text fields mismatch in {}\nactual={:#?}\nexpected={:#?}",
        expectation.file, analysis.final_fields, expected
    );
}

#[test]
fn dictionary_dark_text_elements() {
    assert_image_expectation(&ImageExpectation {
        file: "dictionary_default_dark.png",
        expected_final_fields: &[
            "< >",
            "Dictionary",
            "Dictionary",
            "Thesaurus",
            "Apple",
            "Wikipedia",
            "Type a word to look up in...",
            "New Oxford American Dictionary",
            "Q Search",
            "All",
            "A",
        ],
    });
}

#[test]
fn dictionary_light_text_elements() {
    assert_image_expectation(&ImageExpectation {
        file: "dictionary_default_light.png",
        expected_final_fields: &[
            "< >",
            "Dictionary",
            "Dictionary",
            "Thesaurus",
            "Apple",
            "Wikipedia",
            "Type a word to look up in...",
            "New Oxford American Dictionary",
            "Q Search",
            "All",
            "A",
        ],
    });
}

#[test]
fn messages_dark_text_elements() {
    assert_image_expectation(&ImageExpectation {
        file: "messages_default_dark.png",
        expected_final_fields: &[
            "Sign in to iMessage with your Apple Account",
            "Sign in with your Apple Account to send unlimited messages to any device right from your Mac.",
            "Email or Phone Number",
            "Forgot password?",
            "Create New Apple Account",
            "Cancel",
            "Sign In",
        ],
    });
}

#[test]
fn messages_light_text_elements() {
    assert_image_expectation(&ImageExpectation {
        file: "messages_default_light.png",
        expected_final_fields: &[
            "Sign in to iMessage with your Apple Account",
            "Sign in with your Apple Account to send unlimited messages to any device right from your Mac.",
            "Email or Phone Number-",
            "Forgot password?",
            "Create New Apple Account",
            "Cancel",
            "Sign In",
        ],
    });
}

#[test]
fn facetime_dark_text_elements() {
    assert_image_expectation(&ImageExpectation {
        file: "facetime_default_dark.png",
        expected_final_fields: &[
            "Sign in to FaceTime with your Apple Account",
            "Activate FaceTime to make or receive calls from this Mac",
            "Email or Phone Number",
            "Forgot password?",
            "Create New Apple Account",
            "Cancel",
            "Sign In",
        ],
    });
}

#[test]
fn facetime_light_text_elements() {
    assert_image_expectation(&ImageExpectation {
        file: "facetime_default_light.png",
        expected_final_fields: &[
            "Sign in to FaceTime with your Apple Account",
            "Activate FaceTime to make or receive calls from this Mac",
            "Email or Phone Number",
            "Forgot password?",
            "Create New Apple Account",
            "Cancel",
            "Sign In",
        ],
    });
}
