#[path = "../src/vision/regions.rs"]
mod regions;

use std::path::PathBuf;

use desktop_core::protocol::Bounds;
use image::ImageReader;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/settings-plus")
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
fn infers_plus_button_on_vm_accessibility_fixtures() {
    let cases = [
        ("vm-accessibility-empty-right.png", 990.0, 354.0),
        ("vm-accessibility-empty-center.png", 775.0, 331.0),
        ("vm-accessibility-empty-left.png", 320.0, 272.0),
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
fn detects_window_and_content_regions_on_vm_accessibility_fixtures() {
    let cases = [
        (
            "vm-accessibility-empty-right.png",
            (741.0, 214.0, 716.0, 639.0),
            (959.0, 268.3, 498.0, 584.7),
        ),
        (
            "vm-accessibility-empty-center.png",
            (524.0, 193.0, 718.0, 635.0),
            (744.0, 247.0, 498.0, 581.0),
        ),
        (
            "vm-accessibility-empty-left.png",
            (0.0, 49.7, 787.0, 721.3),
            (72.0, 100.0, 715.0, 671.0),
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
fn detects_regions_on_dark_vm_fixtures() {
    let cases = [
        "vm-dark-dark-left.png",
        "vm-dark-dark-right.png",
        "vm-dark-forest-center.png",
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
