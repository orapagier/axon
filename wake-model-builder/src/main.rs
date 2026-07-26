//! Builds a personal rustpotter wake-word "reference" model from a handful of
//! WAV takes — the same builder `rustpotter-cli build` and axon-voice's
//! on-device JNI bridge use, just invoked as a subprocess by axon-agent's
//! `/api/wakeword/build` (see that module's doc comment for why: linking
//! `rustpotter` straight into axon-agent pulls in candle-core's stale pinned
//! deps and breaks the build).
//!
//! Usage: wake-model-builder <name> <threshold> <avg_threshold> <mfcc_size> <out_path> <sample.wav>...
//! Exits non-zero with a message on stderr on failure.

use rustpotter::{WakewordRef, WakewordRefBuildFromFiles, WakewordSave};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [name, threshold, avg_threshold, mfcc_size, out_path, samples @ ..] = args.as_slice()
    else {
        eprintln!(
            "usage: wake-model-builder <name> <threshold> <avg_threshold> <mfcc_size> <out_path> <sample.wav>..."
        );
        return ExitCode::FAILURE;
    };

    let threshold: f32 = match threshold.parse() {
        Ok(v) => v,
        Err(_) => {
            eprintln!("invalid threshold: {threshold}");
            return ExitCode::FAILURE;
        }
    };
    let avg_threshold: f32 = match avg_threshold.parse() {
        Ok(v) => v,
        Err(_) => {
            eprintln!("invalid avg_threshold: {avg_threshold}");
            return ExitCode::FAILURE;
        }
    };
    let mfcc_size: u16 = match mfcc_size.parse() {
        Ok(v) => v,
        Err(_) => {
            eprintln!("invalid mfcc_size: {mfcc_size}");
            return ExitCode::FAILURE;
        }
    };

    let wakeword = match WakewordRef::new_from_sample_files(
        name.clone(),
        Some(threshold),
        Some(avg_threshold),
        samples.to_vec(),
        mfcc_size,
    ) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("build failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    match wakeword.save_to_file(out_path) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("save failed: {e}");
            ExitCode::FAILURE
        }
    }
}
