use std::path::PathBuf;

use image::ImageReader;
use serde_json::json;

#[path = "../vision/ocr.rs"]
mod ocr;
#[path = "../trace.rs"]
mod trace;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: cargo run -p desktopctld --bin ocr_dump -- <image.png> [needle]");
        std::process::exit(2);
    }

    let image_path = PathBuf::from(&args[1]);
    let needle = args.get(2).map(|s| s.to_lowercase());

    let image = ImageReader::open(&image_path)?.decode()?;
    let width = image.width();
    let height = image.height();
    let texts = ocr::recognize_text_from_image(&image_path, width, height)?;

    println!(
        "{}",
        json!({
            "image": image_path,
            "width": width,
            "height": height,
            "count": texts.len()
        })
    );

    for (idx, t) in texts.iter().enumerate() {
        if let Some(needle) = &needle {
            if !t.text.to_lowercase().contains(needle) {
                continue;
            }
        }
        println!(
            "{}",
            json!({
                "n": idx + 1,
                "text": t.text,
                "confidence": t.confidence,
                "bounds": t.bounds,
            })
        );
    }

    Ok(())
}
