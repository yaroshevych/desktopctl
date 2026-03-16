#[path = "../src/vision/regions.rs"]
mod regions;

use std::path::PathBuf;

use desktop_core::protocol::Bounds;
use image::ImageReader;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/settings-screenshots")
        .join(name)
}

fn infer_add_click_from_regions(regions: &regions::SettingsRegions) -> Option<(f64, f64)> {
    let table = regions.table_bounds.as_ref()?;
    Some((table.x + 12.0, table.y + table.height - 8.0))
}

fn assert_bounds_close(actual: &Bounds, expected: (f64, f64, f64, f64), tol: f64, label: &str) {
    let (ex, ey, ew, eh) = expected;
    assert!(
        (actual.x - ex).abs() <= tol,
        "{label} x mismatch: expected {ex:.1}, got {:.1}",
        actual.x
    );
    assert!(
        (actual.y - ey).abs() <= tol,
        "{label} y mismatch: expected {ey:.1}, got {:.1}",
        actual.y
    );
    assert!(
        (actual.width - ew).abs() <= tol,
        "{label} width mismatch: expected {ew:.1}, got {:.1}",
        actual.width
    );
    assert!(
        (actual.height - eh).abs() <= tol,
        "{label} height mismatch: expected {eh:.1}, got {:.1}",
        actual.height
    );
}

fn assert_bounds_inside(inner: &Bounds, outer: &Bounds, tol: f64, label: &str) {
    assert!(
        inner.x >= outer.x - tol,
        "{label} left edge should be inside outer bounds: inner.x={:.1}, outer.x={:.1}",
        inner.x,
        outer.x
    );
    assert!(
        inner.y >= outer.y - tol,
        "{label} top edge should be inside outer bounds: inner.y={:.1}, outer.y={:.1}",
        inner.y,
        outer.y
    );
    assert!(
        inner.x + inner.width <= outer.x + outer.width + tol,
        "{label} right edge should be inside outer bounds: inner.right={:.1}, outer.right={:.1}",
        inner.x + inner.width,
        outer.x + outer.width
    );
    assert!(
        inner.y + inner.height <= outer.y + outer.height + tol,
        "{label} bottom edge should be inside outer bounds: inner.bottom={:.1}, outer.bottom={:.1}",
        inner.y + inner.height,
        outer.y + outer.height
    );
}

#[test]
fn infers_plus_button_on_settings_screenshots() {
    let cases = [
        ("dark-dark-left.png", 326.0, 362.0),
        ("light-dark-center.png", 632.0, 242.0),
        ("light-forest-right.png", 999.0, 347.0),
    ];
    let mut failures = Vec::new();

    for (name, expected_x, expected_y) in cases {
        let path = fixture_path(name);
        let image = ImageReader::open(&path)
            .expect("open fixture")
            .decode()
            .expect("decode fixture")
            .to_rgba8();
        let detected = regions::detect_settings_regions(&image);
        let (actual_x, actual_y) =
            infer_add_click_from_regions(&detected).expect("add click should be inferred");
        if (actual_x - expected_x).abs() > 12.0 || (actual_y - expected_y).abs() > 12.0 {
            failures.push(format!(
                "{} expected=({:.1},{:.1}) actual=({:.1},{:.1})",
                name, expected_x, expected_y, actual_x, actual_y
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "fixture mismatches:\n{}",
        failures.join("\n")
    );
}

#[test]
fn detects_window_and_content_regions_on_settings_screenshots() {
    let cases = [
        (
            "dark-dark-left.png",
            (69.0, 231.0, 733.0, 625.0),
            (294.0, 284.1, 508.0, 571.9),
        ),
        (
            "light-dark-center.png",
            (382.0, 112.0, 715.0, 625.0),
            (601.0, 165.1, 496.0, 571.9),
        ),
        (
            "light-forest-right.png",
            (751.0, 217.0, 715.0, 625.0),
            (968.0, 270.1, 498.0, 571.9),
        ),
    ];

    for (name, expected_window, expected_content) in cases {
        let path = fixture_path(name);
        let image = ImageReader::open(&path)
            .expect("open fixture")
            .decode()
            .expect("decode fixture")
            .to_rgba8();
        let detected = regions::detect_settings_regions(&image);
        let window = detected
            .window_bounds
            .as_ref()
            .expect("window bounds should exist");
        let content = detected
            .content_bounds
            .as_ref()
            .expect("content bounds should exist");
        assert_bounds_close(window, expected_window, 20.0, "window");
        assert_bounds_close(content, expected_content, 20.0, "content");
    }
}

#[test]
fn detects_regions_on_dark_settings_screenshots() {
    let cases = [
        "dark-dark-left.png",
        "dark-dark-right.png",
        "dark-forest-center.png",
    ];

    for name in cases {
        let path = fixture_path(name);
        let image = ImageReader::open(&path)
            .expect("open fixture")
            .decode()
            .expect("decode fixture")
            .to_rgba8();
        let detected = regions::detect_settings_regions(&image);
        let window = detected
            .window_bounds
            .as_ref()
            .expect("window bounds should exist");
        let content = detected
            .content_bounds
            .as_ref()
            .expect("content bounds should exist");
        let table = detected
            .table_bounds
            .as_ref()
            .expect("table bounds should exist");

        assert_bounds_inside(content, window, 4.0, "content");
        assert_bounds_inside(table, content, 24.0, "table");
        assert!(window.width >= 600.0, "window width too small for {name}");
        assert!(window.height >= 500.0, "window height too small for {name}");
        assert!(content.width >= 450.0, "content width too small for {name}");
        assert!(content.height >= 500.0, "content height too small for {name}");
    }
}
