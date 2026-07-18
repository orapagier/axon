package com.axon.voice.api

import com.axon.voice.Prefs
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.MultipartBody
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import org.json.JSONObject
import java.io.File
import java.util.concurrent.TimeUnit

/**
 * Blocking HTTP client for the Axon backend (call from worker threads).
 * Speaks the same three endpoints the dashboard uses:
 *   GET  /api/health                — connectivity probe
 *   POST /api/audio/transcribe      — multipart clip -> {ok, text}
 *   POST /api/audio/speech          — {text} -> audio bytes (any format MediaPlayer handles)
 */
class AxonClient(private val prefs: Prefs) {

    val http: OkHttpClient = OkHttpClient.Builder()
        .connectTimeout(10, TimeUnit.SECONDS)
        .readTimeout(120, TimeUnit.SECONDS)
        .pingInterval(25, TimeUnit.SECONDS)
        .build()

    private fun request(path: String): Request.Builder =
        Request.Builder()
            .url(prefs.baseUrl + path)
            .header("Authorization", "Bearer " + prefs.masterKey)

    fun health(): Boolean = try {
        http.newCall(request("/api/health").build()).execute().use { it.isSuccessful }
    } catch (_: Exception) {
        false
    }

    /** WAV clip in, transcript out. Throws with a readable message on failure. */
    fun transcribe(wav: ByteArray): String {
        val body = MultipartBody.Builder()
            .setType(MultipartBody.FORM)
            .addFormDataPart(
                "file", "clip.wav",
                wav.toRequestBody("audio/wav".toMediaType())
            )
            .build()
        http.newCall(request("/api/audio/transcribe").post(body).build()).execute().use { res ->
            val text = res.body?.string() ?: ""
            val json = runCatching { JSONObject(text) }.getOrNull()
                ?: throw RuntimeException("transcribe failed (${res.code})")
            if (json.optBoolean("ok")) return json.optString("text", "").trim()
            throw RuntimeException(json.optString("error", "transcribe failed (${res.code})"))
        }
    }

    /** Synthesize speech into [out]. False means "no server TTS" — the caller
     *  falls back to Android's built-in TextToSpeech, like the dashboard falls
     *  back to browser speechSynthesis. */
    fun speech(text: String, out: File): Boolean = try {
        val body = JSONObject().put("text", text).toString()
            .toRequestBody("application/json".toMediaType())
        http.newCall(request("/api/audio/speech").post(body).build()).execute().use { res ->
            if (!res.isSuccessful) return false
            res.body?.byteStream()?.use { input ->
                out.outputStream().use { input.copyTo(it) }
            } ?: return false
            out.length() > 0
        }
    } catch (_: Exception) {
        false
    }
}
