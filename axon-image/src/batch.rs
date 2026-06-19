use crate::{
    canvas::{self, OutputFormat},
    error::Result,
};
use image::DynamicImage;
use rayon::prelude::*;
use std::path::Path;

/// Result of processing a single item in a batch
#[derive(Debug)]
pub struct BatchResult {
    pub input: String,
    pub output: Option<String>,
    pub error: Option<String>,
    pub duration_ms: u64,
}

impl BatchResult {
    pub fn success(input: String, output: String, duration_ms: u64) -> Self {
        Self {
            input,
            output: Some(output),
            error: None,
            duration_ms,
        }
    }

    pub fn failure(input: String, error: String, duration_ms: u64) -> Self {
        Self {
            input,
            output: None,
            error: Some(error),
            duration_ms,
        }
    }

    pub fn is_success(&self) -> bool {
        self.error.is_none()
    }
}

/// Summary statistics for a batch run
#[derive(Debug)]
pub struct BatchSummary {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub total_duration_ms: u64,
    pub avg_duration_ms: u64,
    pub failures: Vec<(String, String)>, // (input, error)
}

impl BatchSummary {
    pub fn from_results(results: &[BatchResult]) -> Self {
        let total = results.len();
        let succeeded = results.iter().filter(|r| r.is_success()).count();
        let failed = total - succeeded;
        let total_duration_ms: u64 = results.iter().map(|r| r.duration_ms).sum();
        let avg_duration_ms = if total > 0 {
            total_duration_ms / total as u64
        } else {
            0
        };
        let failures = results
            .iter()
            .filter(|r| !r.is_success())
            .map(|r| (r.input.clone(), r.error.clone().unwrap_or_default()))
            .collect();

        Self {
            total,
            succeeded,
            failed,
            total_duration_ms,
            avg_duration_ms,
            failures,
        }
    }

    pub fn print_report(&self) {
        println!("═══ Batch Processing Report ═══");
        println!("  Total:     {}", self.total);
        println!("  Succeeded: {}", self.succeeded);
        println!("  Failed:    {}", self.failed);
        println!("  Total time: {}ms", self.total_duration_ms);
        println!("  Avg time:   {}ms/image", self.avg_duration_ms);
        if !self.failures.is_empty() {
            println!("\n  Failures:");
            for (file, err) in &self.failures {
                println!("    ✗ {}: {}", file, err);
            }
        }
    }
}

/// Process a list of input paths using a provided function in parallel.
///
/// # Parameters
/// - `input_paths`: list of image file paths to process
/// - `output_dir`: directory where processed files will be saved
/// - `output_format`: format to encode output images as
/// - `processor`: closure that transforms a DynamicImage
///
/// # Example
/// ```rust
/// let results = process_files(
///     &inputs,
///     "output/",
///     OutputFormat::Jpeg,
///     |img| Ok(img.blur(2.0)),
/// );
/// ```
pub fn process_files<F>(
    input_paths: &[String],
    output_dir: &str,
    output_format: OutputFormat,
    processor: F,
) -> (Vec<BatchResult>, BatchSummary)
where
    F: Fn(DynamicImage) -> Result<DynamicImage> + Send + Sync,
{
    std::fs::create_dir_all(output_dir).ok();

    let results: Vec<BatchResult> = input_paths
        .par_iter()
        .map(|input_path| {
            let start = std::time::Instant::now();

            let result = (|| -> Result<String> {
                let img = canvas::load(input_path)?;
                let processed = processor(img)?;

                let input_file = Path::new(input_path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("output");

                let output_path = format!(
                    "{}/{}.{}",
                    output_dir,
                    input_file,
                    output_format.extension()
                );

                canvas::save_as(&processed, &output_path, output_format)?;
                Ok(output_path)
            })();

            let duration_ms = start.elapsed().as_millis() as u64;

            match result {
                Ok(output) => BatchResult::success(input_path.clone(), output, duration_ms),
                Err(e) => BatchResult::failure(input_path.clone(), e.to_string(), duration_ms),
            }
        })
        .collect();

    let summary = BatchSummary::from_results(&results);
    (results, summary)
}

/// Process images in-memory without saving to disk.
/// Returns processed images alongside their original paths.
pub fn process_in_memory<F>(
    input_paths: &[String],
    processor: F,
) -> Vec<(String, Result<DynamicImage>)>
where
    F: Fn(DynamicImage) -> Result<DynamicImage> + Send + Sync,
{
    input_paths
        .par_iter()
        .map(|path| {
            let result = canvas::load(path).and_then(|img| processor(img));
            (path.clone(), result)
        })
        .collect()
}

/// Process raw byte buffers in parallel (useful when images come from network/API)
pub fn process_buffers<F>(
    inputs: Vec<(String, Vec<u8>)>, // (label, bytes)
    output_format: OutputFormat,
    processor: F,
) -> Vec<(String, Result<Vec<u8>>)>
where
    F: Fn(DynamicImage) -> Result<DynamicImage> + Send + Sync,
{
    inputs
        .into_par_iter()
        .map(|(label, bytes)| {
            let result = canvas::from_bytes(&bytes)
                .and_then(|img| processor(img))
                .and_then(|out| canvas::to_bytes(&out, output_format));
            (label, result)
        })
        .collect()
}

/// Scan a directory for image files and return their paths
pub fn find_images(dir: &str, recursive: bool) -> Vec<String> {
    let extensions = ["jpg", "jpeg", "png", "webp", "bmp", "tiff", "tif"];
    let mut paths = Vec::new();

    fn collect(dir: &Path, recursive: bool, exts: &[&str], paths: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && recursive {
                collect(&path, recursive, exts, paths);
            } else if path.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if exts.contains(&ext.to_lowercase().as_str()) {
                        if let Some(s) = path.to_str() {
                            paths.push(s.to_string());
                        }
                    }
                }
            }
        }
    }

    collect(Path::new(dir), recursive, &extensions, &mut paths);
    paths.sort();
    paths
}

/// Set the number of threads used for batch processing (default: number of logical CPUs)
pub fn set_thread_count(count: usize) {
    rayon::ThreadPoolBuilder::new()
        .num_threads(count)
        .build_global()
        .ok();
}
