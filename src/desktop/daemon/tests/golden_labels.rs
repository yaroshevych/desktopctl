//! Integration test: loads golden/manifest.json, runs detector on each image,
//! computes per-category recall at appropriate IoU thresholds.
//! Uses detect_ui_boxes(image) — same as runtime with no OCR text seeds.

#[path = "../src/vision/metal_pipeline.rs"]
#[allow(dead_code)]
mod metal_pipeline;

#[path = "../src/vision/tokenize_boxes.rs"]
#[allow(dead_code)]
mod tokenize_boxes;

#[path = "../src/vision/ocr.rs"]
mod ocr;

#[path = "../src/trace.rs"]
mod trace;

use std::{collections::HashMap, fs, path::PathBuf};

use desktop_core::protocol::Bounds;
use image::{ImageReader, RgbaImage};
use serde::Deserialize;

// Path to the manifest relative to CARGO_MANIFEST_DIR.
const MANIFEST_REL: &str = "tests/fixtures/golden/manifest.json";

// Per-category IoU thresholds for a GT box to count as matched.
struct CategoryConfig {
    iou_threshold: f64,
    // "box" matches all box predictions; "glyph" also counts glyph predictions.
    match_glyphs: bool,
    min_recall: f64,
}

fn category_configs() -> HashMap<&'static str, CategoryConfig> {
    let mut m = HashMap::new();
    m.insert(
        "text_field",
        CategoryConfig {
            iou_threshold: 0.50,
            match_glyphs: false,
            min_recall: 0.30,
        },
    );
    m.insert(
        "container",
        CategoryConfig {
            iou_threshold: 0.40,
            match_glyphs: false,
            min_recall: 0.10,
        },
    );
    m.insert(
        "text_or_paragraph",
        CategoryConfig {
            iou_threshold: 0.35,
            match_glyphs: false,
            min_recall: 0.20,
        },
    );
    m.insert(
        "button",
        CategoryConfig {
            iou_threshold: 0.40,
            match_glyphs: false,
            min_recall: 0.20,
        },
    );
    m.insert(
        "icon",
        CategoryConfig {
            iou_threshold: 0.35,
            match_glyphs: true,
            min_recall: 0.10,
        },
    );
    m.insert(
        "list",
        CategoryConfig {
            iou_threshold: 0.40,
            match_glyphs: false,
            min_recall: 0.10,
        },
    );
    m
}

// ── manifest schema ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct Manifest {
    items: Vec<ManifestItem>,
}

#[derive(Debug, Deserialize)]
struct ManifestItem {
    id: String,
    /// Path relative to eval/datasets/ (e.g. "labels/text_fields/<id>/image.png").
    image_rel_path: String,
    annotations: Vec<GtAnnotation>,
}

#[derive(Debug, Deserialize)]
struct GtAnnotation {
    id: String,
    category: String,
    bbox: [f64; 4],
}

// ── helpers ──────────────────────────────────────────────────────────────────

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

fn to_bounds(bbox: [f64; 4]) -> Option<Bounds> {
    let w = bbox[2].max(0.0);
    let h = bbox[3].max(0.0);
    if w < 2.0 || h < 2.0 {
        return None;
    }
    Some(Bounds {
        x: bbox[0],
        y: bbox[1],
        width: w,
        height: h,
    })
}

// ── test ─────────────────────────────────────────────────────────────────────

#[test]
fn golden_labels_per_category_recall() {
    let manifest_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(MANIFEST_REL);
    assert!(
        manifest_path.exists(),
        "golden manifest not found: {} — run eval/run/export_golden_manifest.py first",
        manifest_path.display()
    );

    let raw = fs::read_to_string(&manifest_path).expect("read manifest");
    let manifest: Manifest = serde_json::from_str(&raw).expect("parse manifest JSON");
    assert!(!manifest.items.is_empty(), "manifest has no items");

    // Resolve image paths: manifest stores paths relative to eval/datasets/.
    // CARGO_MANIFEST_DIR = src/desktop/daemon → repo root is ../../..
    let datasets_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join("eval/datasets");

    let configs = category_configs();

    // per-category counters: matched, total
    let mut matched: HashMap<String, usize> = HashMap::new();
    let mut total: HashMap<String, usize> = HashMap::new();

    // global predicted count for ratio check
    let mut global_predicted = 0usize;
    let mut global_gt = 0usize;

    for item in &manifest.items {
        let image_path = datasets_dir.join(&item.image_rel_path);
        let image = ImageReader::open(&image_path)
            .unwrap_or_else(|e| panic!("open image {}: {}", image_path.display(), e))
            .decode()
            .unwrap_or_else(|e| panic!("decode image {}: {}", image_path.display(), e))
            .to_rgba8();

        let predicted = tokenize_boxes::detect_ui_boxes(&image);
        let glyph_text_bounds: Vec<Bounds> = Vec::new();
        let predicted_glyphs = tokenize_boxes::detect_glyphs(&image, &glyph_text_bounds);

        global_predicted += predicted.len();
        global_gt += item.annotations.len();

        for ann in &item.annotations {
            let cat = ann.category.as_str();
            let Some(gt) = to_bounds(ann.bbox) else {
                continue;
            };

            *total.entry(ann.category.clone()).or_default() += 1;

            let cfg = configs
                .get(cat)
                .unwrap_or_else(|| panic!("unknown category '{}' in annotation {}", cat, ann.id));

            let hit_box = predicted.iter().any(|p| iou(p, &gt) >= cfg.iou_threshold);
            let hit = if cfg.match_glyphs && !hit_box {
                predicted_glyphs
                    .iter()
                    .any(|p| iou(p, &gt) >= cfg.iou_threshold)
            } else {
                hit_box
            };

            if hit {
                *matched.entry(ann.category.clone()).or_default() += 1;
            } else {
                let best = predicted
                    .iter()
                    .map(|p| iou(p, &gt))
                    .fold(0.0_f64, f64::max);
                eprintln!(
                    "  MISS {:<22} {:<60} bbox=[{:.0},{:.0},{:.0},{:.0}] best_iou={:.3}",
                    cat, item.id, gt.x, gt.y, gt.width, gt.height, best
                );
            }
        }
    }

    // Print summary
    println!("\n=== golden_labels per-category recall ===");
    println!(
        "{:<22} {:>8} {:>8} {:>8}  gate",
        "category", "matched", "total", "recall"
    );
    println!("{}", "-".repeat(56));

    let ordered_cats = [
        "text_field",
        "container",
        "text_or_paragraph",
        "button",
        "icon",
        "list",
    ];
    for cat in &ordered_cats {
        let t = *total.get(*cat).unwrap_or(&0);
        let m = *matched.get(*cat).unwrap_or(&0);
        let recall = if t == 0 { 0.0 } else { m as f64 / t as f64 };
        let gate = configs[cat].min_recall;
        println!(
            "{:<22} {:>8} {:>8} {:>8.3}  >= {:.2}",
            cat, m, t, recall, gate
        );
    }
    let pred_ratio = if global_gt == 0 {
        0.0
    } else {
        global_predicted as f64 / global_gt as f64
    };
    println!("{}", "-".repeat(56));
    println!(
        "global: predicted={} gt={} ratio={:.2}",
        global_predicted, global_gt, pred_ratio
    );

    // Assert recall gates
    for cat in &ordered_cats {
        let t = *total.get(*cat).unwrap_or(&0);
        let m = *matched.get(*cat).unwrap_or(&0);
        let recall = if t == 0 { 0.0 } else { m as f64 / t as f64 };
        let gate = configs[cat].min_recall;
        assert!(
            recall >= gate,
            "{} recall too low: {:.3} ({}/{}) — gate >= {:.2}",
            cat,
            recall,
            m,
            t,
            gate
        );
    }
}

// ── single-image debug tests ────────────────────────────────────────────────

fn debug_single_image(id_substr: &str) {
    let manifest_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(MANIFEST_REL);
    let raw = fs::read_to_string(&manifest_path).expect("read manifest");
    let manifest: Manifest = serde_json::from_str(&raw).expect("parse manifest JSON");
    let datasets_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join("eval/datasets");

    let configs = category_configs();

    let item = manifest
        .items
        .iter()
        .find(|i| i.id.contains(id_substr))
        .unwrap_or_else(|| panic!("{} not found in manifest", id_substr));

    let image_path = datasets_dir.join(&item.image_rel_path);
    let image = ImageReader::open(&image_path)
        .unwrap_or_else(|e| panic!("open image {}: {}", image_path.display(), e))
        .decode()
        .unwrap_or_else(|e| panic!("decode image {}: {}", image_path.display(), e))
        .to_rgba8();

    // Run OCR (same as production path).
    let texts = match ocr::recognize_text_from_image(
        &image_path,
        image.width(),
        image.height(),
    ) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("warn: OCR failed: {} — falling back to no-text path", e.message);
            Vec::new()
        }
    };
    let text_bounds: Vec<Bounds> = texts.iter().map(|t| t.bounds.clone()).collect();

    println!("\n=== {}: {} OCR texts ===", id_substr, texts.len());
    for (i, t) in texts.iter().enumerate() {
        println!(
            "  text {:3}: x={:6.1} y={:6.1} w={:6.1} h={:6.1} conf={:.2} {:?}",
            i, t.bounds.x, t.bounds.y, t.bounds.width, t.bounds.height,
            t.confidence, t.text
        );
    }

    let text_labels: Vec<&str> = texts.iter().map(|t| t.text.as_str()).collect();
    let predicted = tokenize_boxes::detect_ui_boxes_with_labels(&image, &text_bounds, &text_labels);
    let predicted_glyphs = tokenize_boxes::detect_glyphs(&image, &text_bounds);

    println!("\n=== {} predicted boxes ===", predicted.len());
    for (i, p) in predicted.iter().enumerate() {
        println!(
            "  box {:3}: x={:6.1} y={:6.1} w={:6.1} h={:6.1}",
            i, p.x, p.y, p.width, p.height
        );
    }
    println!("=== {} predicted glyphs ===", predicted_glyphs.len());

    println!("\n=== GT annotations ===");
    for ann in &item.annotations {
        let gt = to_bounds(ann.bbox);
        let gt = match gt {
            Some(g) => g,
            None => continue,
        };

        let best_iou = predicted
            .iter()
            .map(|p| iou(p, &gt))
            .fold(0.0_f64, f64::max);
        let best_glyph_iou = predicted_glyphs
            .iter()
            .map(|p| iou(p, &gt))
            .fold(0.0_f64, f64::max);

        let cfg = &configs[ann.category.as_str()];
        let hit = best_iou >= cfg.iou_threshold
            || (cfg.match_glyphs && best_glyph_iou >= cfg.iou_threshold);

        println!(
            "  {} {:<22} bbox=[{:6.1},{:6.1},{:6.1},{:6.1}] best_iou={:.3} glyph_iou={:.3} {}",
            if hit { "HIT " } else { "MISS" },
            ann.category,
            gt.x, gt.y, gt.width, gt.height,
            best_iou, best_glyph_iou,
            ann.id
        );
    }

    // Generate overlay
    let out_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join("tmp/golden_overlays");
    fs::create_dir_all(&out_dir).expect("create output dir");

    let green = [0u8, 220, 0, 255];  // predicted boxes
    let red = [220u8, 0, 0, 255];    // GT boxes
    let blue = [60u8, 120, 255, 255]; // predicted glyphs
    let cyan = [0u8, 220, 220, 255];  // OCR texts

    let mut overlay = image.clone();
    for t in &texts {
        draw_rect(&mut overlay, &t.bounds, cyan, 1);
    }
    for p in &predicted {
        draw_rect(&mut overlay, p, green, 2);
    }
    for p in &predicted_glyphs {
        draw_rect(&mut overlay, p, blue, 1);
    }
    let hide_gt = std::env::var("HIDE_GT").is_ok();
    if !hide_gt {
        for ann in &item.annotations {
            if let Some(gt) = to_bounds(ann.bbox) {
                draw_rect(&mut overlay, &gt, red, 2);
            }
        }
    }

    let out_path = out_dir.join(format!("debug_{}.png", id_substr));
    overlay.save(&out_path).expect("save overlay");
    println!("\nOverlay: {}", out_path.display());
}

#[test]
fn debug_dictionary() {
    debug_single_image("dictionary");
}

#[test]
fn debug_messages() {
    debug_single_image("messages");
}

// ── overlay generation ──────────────────────────────────────────────────────

fn draw_rect(img: &mut RgbaImage, b: &Bounds, color: [u8; 4], thickness: u32) {
    let w = img.width();
    let h = img.height();
    let x1 = (b.x as u32).min(w.saturating_sub(1));
    let y1 = (b.y as u32).min(h.saturating_sub(1));
    let x2 = ((b.x + b.width) as u32).min(w.saturating_sub(1));
    let y2 = ((b.y + b.height) as u32).min(h.saturating_sub(1));
    for t in 0..thickness {
        let x1t = x1.saturating_sub(t);
        let y1t = y1.saturating_sub(t);
        let x2t = (x2 + t).min(w - 1);
        let y2t = (y2 + t).min(h - 1);
        for x in x1t..=x2t {
            img.put_pixel(x, y1t, image::Rgba(color));
            img.put_pixel(x, y2t, image::Rgba(color));
        }
        for y in y1t..=y2t {
            img.put_pixel(x1t, y, image::Rgba(color));
            img.put_pixel(x2t, y, image::Rgba(color));
        }
    }
}

#[test]
#[ignore] // Run with: cargo test -p desktopctld --test golden_labels -- --ignored generate_overlays
fn generate_overlays() {
    let manifest_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(MANIFEST_REL);
    let raw = fs::read_to_string(&manifest_path).expect("read manifest");
    let manifest: Manifest = serde_json::from_str(&raw).expect("parse manifest JSON");
    let datasets_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join("eval/datasets");

    let out_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join("tmp/golden_overlays");
    fs::create_dir_all(&out_dir).expect("create output dir");

    let green = [0u8, 220, 0, 255]; // predicted boxes
    let red = [220u8, 0, 0, 255]; // GT boxes
    let blue = [60u8, 120, 255, 255]; // predicted glyphs

    // Filter to specific images if OVERLAY_FILTER env var is set (comma-separated substrings).
    let filter = std::env::var("OVERLAY_FILTER").unwrap_or_default();
    let filters: Vec<&str> = if filter.is_empty() {
        Vec::new()
    } else {
        filter.split(',').collect()
    };

    for item in &manifest.items {
        if !filters.is_empty() && !filters.iter().any(|f| item.id.contains(f)) {
            continue;
        }
        let image_path = datasets_dir.join(&item.image_rel_path);
        let image = ImageReader::open(&image_path)
            .unwrap_or_else(|e| panic!("open image {}: {}", image_path.display(), e))
            .decode()
            .unwrap_or_else(|e| panic!("decode image {}: {}", image_path.display(), e))
            .to_rgba8();

        let predicted = tokenize_boxes::detect_ui_boxes(&image);
        let glyph_text_bounds: Vec<Bounds> = Vec::new();
        let predicted_glyphs = tokenize_boxes::detect_glyphs(&image, &glyph_text_bounds);

        let mut overlay = image.clone();
        for p in &predicted {
            draw_rect(&mut overlay, p, green, 1);
        }
        for p in &predicted_glyphs {
            draw_rect(&mut overlay, p, blue, 1);
        }
        let hide_gt = std::env::var("HIDE_GT").is_ok();
        if !hide_gt {
            for ann in &item.annotations {
                if let Some(gt) = to_bounds(ann.bbox) {
                    draw_rect(&mut overlay, &gt, red, 2);
                }
            }
        }

        eprintln!(
            "  {} → {} boxes, {} glyphs",
            item.id,
            predicted.len(),
            predicted_glyphs.len()
        );

        // Use short filename from id
        let short_id: String = item.id.chars().take(60).collect();
        let out_path = out_dir.join(format!("{short_id}.png"));
        overlay.save(&out_path).expect("save overlay");
    }
    println!("Overlays written to: {}", out_dir.display());
}
