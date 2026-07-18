# axon-voice

Voice-first Android client for Axon. A single-screen app: tap the orb (or say
"Hey Axon", or long-press the power button) and talk to the same agent, tools,
and memory the dashboard uses — the backend is unchanged.

## How it talks to the server

Everything rides the existing dashboard API with `Authorization: Bearer
<AXON_MASTER_KEY>`:

| Purpose | Endpoint |
|---|---|
| Chat | `wss://host/ws?api_key=…` → `{task, session_id}` in, AgentEvent frames out |
| Speech-to-text | `POST /api/audio/transcribe` (multipart WAV) |
| Text-to-speech | `POST /api/audio/speech` (`{text}` → audio stream) |
| Connectivity probe | `GET /api/health` |

The app records 16 kHz mono WAV, so the server's `stt.*` / `tts.*` settings
must be configured (same requirement as dashboard voice input/replies).
Replies fall back to Android's built-in TextToSpeech when server TTS fails.

## The three invocation paths

1. **Push-to-talk** — tap the orb; capture auto-stops on the ported
   silence-watcher thresholds (1.4 s quiet / 5 s no speech / 12 s cap).
2. **"Hey Axon" always-on** — the in-app toggle starts a microphone foreground
   service running rustpotter (same `heyaxon.rpw` model as the dashboard,
   `spot -g -e -t 0.47` tuning). Wake → "Yes?" → command → spoken reply →
   "Anything else?" follow-up window with the raised 2× bystander RMS bar.
   Grant the battery-optimization exemption when prompted or Doze will
   eventually kill it.
3. **Assistant gesture** — pick Axon under *Settings > Apps > Default apps >
   Digital assistant app*; the power-button/assist gesture then opens the orb
   screen already listening (works over the lock screen).

## Building

Requirements: Android SDK (platform 35), NDK, Rust with the
`aarch64-linux-android` target, `cargo-ndk`, and a JDK 17+ (Android Studio's
JBR works).

```powershell
# 1. Wake-word native lib (writes app/src/main/jniLibs/arm64-v8a/librustpotter_jni.so)
$env:ANDROID_NDK_HOME = "$env:LOCALAPPDATA\Android\Sdk\ndk\android-ndk-r27c"
cd rustpotter-jni
cargo ndk -t arm64-v8a -o ../app/src/main/jniLibs build --release

# 2. APK
$env:JAVA_HOME = "C:\Program Files\Android\Android Studio\jbr"
cd ..
.\gradlew.bat :app:assembleDebug
# → app/build/outputs/apk/debug/app-debug.apk
```

Install via `adb install` (or copy the APK to the phone). On first run open
Settings in the app, enter the server URL and master key, and Test connection.

The wake model ships as `app/src/main/assets/heyaxon.rpw` — a copy of
`axon-ui/public/rustpotter/heyaxon.rpw`. Re-copy it if the model is ever
retrained (rebuild kit lives in `Dev/rustpotter-test`).

## Layout

- `app/` — Kotlin app (`com.axon.voice`): `api/` HTTP+WS client, `audio/`
  recorder/player/chime, `wake/` JNI bridge + foreground service, `assist/`
  digital-assistant role services, `ui/` orb screen + settings.
- `rustpotter-jni/` — standalone Rust cdylib wrapping rustpotter 3.x for JNI.
  Deliberately outside the repo's root cargo workspace.
