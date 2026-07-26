//! JNI bridge for the rustpotter wake-word engine, consumed by
//! com.axon.voice.wake.RustpotterNative. Detector defaults are passed in from
//! Kotlin and mirror axon-ui/src/lib/wakeword.js (threshold 0.47, score_ref
//! 0.22, min_scores 10, eager, gain normalization on, band-pass off).
//!
//! Build: cargo ndk -t arm64-v8a -o ../app/src/main/jniLibs build --release

use jni::objects::{JByteArray, JClass, JObjectArray, JShortArray, JString};
use jni::sys::{jboolean, jbyteArray, jfloat, jint, jlong};
use jni::JNIEnv;
use rustpotter::{
    Rustpotter, RustpotterConfig, SampleFormat, ScoreMode, WakewordRef, WakewordRefBuildFromBuffers,
    WakewordSave,
};
use std::collections::HashMap;

#[no_mangle]
pub extern "system" fn Java_com_axon_voice_wake_RustpotterNative_create(
    env: JNIEnv,
    _class: JClass,
    model: JByteArray,
    threshold: jfloat,
    avg_threshold: jfloat,
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
    // Averaged-score gate: the detection window's mean score must also clear
    // this, on top of the single-frame `threshold`. Off (0.0) for the
    // far-field "hey axon" wake word (the false-triggering seen on-device
    // with a positive value).
    config.detector.avg_threshold = avg_threshold;
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

/// Builds a personal wakeword model from a handful of WAV takes — the same
/// "reference" builder `rustpotter-cli build` runs — for on-device enrollment.
/// Returns the serialized `.rpw` bytes, or null on failure (bad audio, or
/// fewer than 3 usable samples).
#[no_mangle]
pub extern "system" fn Java_com_axon_voice_wake_RustpotterNative_build(
    mut env: JNIEnv,
    _class: JClass,
    name: JString,
    samples: JObjectArray,
    threshold: jfloat,
    avg_threshold: jfloat,
    mfcc_size: jint,
) -> jbyteArray {
    let name: String = match env.get_string(&name) {
        Ok(s) => s.into(),
        Err(_) => return std::ptr::null_mut(),
    };

    let len = match env.get_array_length(&samples) {
        Ok(l) => l,
        Err(_) => return std::ptr::null_mut(),
    };

    let mut sample_map: HashMap<String, Vec<u8>> = HashMap::new();
    for i in 0..len {
        let element = match env.get_object_array_element(&samples, i) {
            Ok(e) => e,
            Err(_) => return std::ptr::null_mut(),
        };
        let bytes = match env.convert_byte_array(&JByteArray::from(element)) {
            Ok(b) => b,
            Err(_) => return std::ptr::null_mut(),
        };
        sample_map.insert(format!("take{i}"), bytes);
    }
    if sample_map.len() < 3 {
        return std::ptr::null_mut();
    }

    let wakeword = match WakewordRef::new_from_sample_buffers(
        name,
        Some(threshold),
        Some(avg_threshold),
        sample_map,
        mfcc_size as u16,
    ) {
        Ok(w) => w,
        Err(_) => return std::ptr::null_mut(),
    };
    let bytes = match wakeword.save_to_buffer() {
        Ok(b) => b,
        Err(_) => return std::ptr::null_mut(),
    };
    match env.byte_array_from_slice(&bytes) {
        Ok(arr) => arr.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
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
