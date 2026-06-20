//! Batch processing example: resize + add watermark to all images in a folder.
//!
//! Usage:
//!   cargo run --example batch_process -- --input ./photos --output ./resized

use image_processor::{batch, canvas::OutputFormat, ImagePipeline, Result};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let input_dir = args.get(2).map(|s| s.as_str()).unwrap_or("./input");
    let output_dir = args.get(4).map(|s| s.as_str()).unwrap_or("./output");

    println!("Scanning {} for images...", input_dir);
    let inputs = batch::find_images(input_dir, false);
    println!("Found {} images", inputs.len());

    if inputs.is_empty() {
        println!("No images found. Exiting.");
        return Ok(());
    }

    // Optional: cap thread count to avoid OOM on 1GB RAM server
    batch::set_thread_count(2);

    let (results, summary) = batch::process_files(&inputs, output_dir, OutputFormat::Jpeg, |img| {
        // Example pipeline: resize to 1080p + vignette + watermark text box
        let processed = ImagePipeline::new(img)
            .resize_fill(1080, 1080)
            .vignette(0.4)
            .build();

        Ok(processed)
    });

    summary.print_report();

    for result in &results {
        if let Some(ref out) = result.output {
            println!("✓ {} → {} ({}ms)", result.input, out, result.duration_ms);
        } else if let Some(ref err) = result.error {
            eprintln!("✗ {} failed: {}", result.input, err);
        }
    }

    Ok(())
}
