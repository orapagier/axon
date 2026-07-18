//! JNI bridge for the rustpotter wake-word engine, consumed by
//! com.axon.voice.wake.RustpotterNative. Detector defaults are passed in from
//! Kotlin and mirror axon-ui/src/lib/wakeword.js (threshold 0.47, score_ref
//! 0.22, min_scores 10, eager, gain normalization on, band-pass off).
//!
//! Build: cargo ndk -t arm64-v8a -o ../app/src/main/jniLibs build --release

use jni::objects::{JByteArray, JClass, JShortArray};
use jni::sys::{jboolean, jfloat, jint, jlong};
use jni::JNIEnv;
use rustpotter::{Rustpotter, RustpotterConfig, SampleFormat, ScoreMode};

#[no_mangle]
pub extern "system" fn Java_com_axon_voice_wake_RustpotterNative_create(
    env: JNIEnv,
    _class: JClass,
    model: JByteArray,
    threshold: jfloat,
    score_ref: jfloat,
    min_scores: jint,
    eager: jboolean,
    gain_normalizer: jboolean,
) -> jlong {
    let bytes = match env.convert_byte_array(&model) {
        Ok(b) => b,
        Err(_) => return 0,
    };
    let mut config = RustpotterConfig::default();
    config.fmt.sample_rate = 16000;
    config.fmt.sample_format = SampleFormat::I16;
    config.fmt.channels = 1;
    config.detector.threshold = threshold;
    config.detector.avg_threshold = 0.0;
    config.detector.score_ref = score_ref;
    config.detector.band_size = 5;
    config.detector.min_scores = min_scores as usize;
    config.detector.eager = eager != 0;
    config.detector.score_mode = ScoreMode::Max;
    config.filters.gain_normalizer.enabled = gain_normalizer != 0;
    config.filters.gain_normalizer.min_gain = 0.1;
    config.filters.gain_normalizer.max_gain = 1.0;
    config.filters.band_pass.enabled = false;

    let mut rustpotter = match Rustpotter::new(&config) {
        Ok(r) => r,
        Err(_) => return 0,
    };
    if rustpotter
        .add_wakeword_from_buffer("hey axon", &bytes)
        .is_err()
    {
        return 0;
    }
    Box::into_raw(Box::new(rustpotter)) as jlong
}

#[no_mangle]
pub extern "system" fn Java_com_axon_voice_wake_RustpotterNative_samplesPerFrame(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jint {
    if handle == 0 {
        return 0;
    }
    let rustpotter = unsafe { &*(handle as *const Rustpotter) };
    rustpotter.get_samples_per_frame() as jint
}

/// Feed one frame of 16k mono PCM16. Returns the detection score, -1.0 when
/// nothing fired this frame.
#[no_mangle]
pub extern "system" fn Java_com_axon_voice_wake_RustpotterNative_process(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    samples: JShortArray,
) -> jfloat {
    if handle == 0 {
        return -1.0;
    }
    let rustpotter = unsafe { &mut *(handle as *mut Rustpotter) };
    let len = match env.get_array_length(&samples) {
        Ok(l) => l as usize,
        Err(_) => return -1.0,
    };
    let mut buf = vec![0i16; len];
    if env.get_short_array_region(&samples, 0, &mut buf).is_err() {
        return -1.0;
    }
    match rustpotter.process_samples(buf) {
        Some(detection) => detection.score,
        None => -1.0,
    }
}

#[no_mangle]
pub extern "system" fn Java_com_axon_voice_wake_RustpotterNative_destroy(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if handle != 0 {
        unsafe { drop(Box::from_raw(handle as *mut Rustpotter)) };
    }
}
