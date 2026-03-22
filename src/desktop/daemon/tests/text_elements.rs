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

use std::path::{Path, PathBuf};

use image::ImageReader;
use std::collections::HashMap;
use text_group::{
    build_words_from_ocr, final_text_fields, group_lines_into_paragraphs, group_words_into_lines,
};

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
    repo_root()
        .join("src/desktop/daemon/tests/fixtures/golden")
        .join(file)
}

fn normalize(s: &str) -> String {
    s.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
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

    let words = build_words_from_ocr(&texts, &frame);
    let lines = group_words_into_lines(&words);
    let paragraphs = group_lines_into_paragraphs(&lines);
    let final_fields: Vec<String> = final_text_fields(&lines, &paragraphs)
        .iter()
        .map(|t| normalize(&t.text))
        .collect();

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
