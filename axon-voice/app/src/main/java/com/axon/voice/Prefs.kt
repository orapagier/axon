package com.axon.voice

import android.content.Context
import java.net.URLEncoder
import java.util.UUID

/**
 * App settings. Stored in app-private SharedPreferences; `allowBackup=false`
 * in the manifest keeps the master key out of cloud backups.
 */
class Prefs(ctx: Context) {
    private val sp = ctx.getSharedPreferences("axon", Context.MODE_PRIVATE)

    var baseUrl: String
        get() = (sp.getString("base_url", "") ?: "").trim().trimEnd('/')
        set(v) {
            sp.edit().putString("base_url", v.trim().trimEnd('/')).apply()
        }

    var masterKey: String
        get() = sp.getString("master_key", "") ?: ""
        set(v) {
            sp.edit().putString("master_key", v.trim()).apply()
        }

    var wakeEnabled: Boolean
        get() = sp.getBoolean("wake_enabled", false)
        set(v) {
            sp.edit().putBoolean("wake_enabled", v).apply()
        }

    /** Whether talking over a spoken reply interrupts it (barge-in). On by
     *  default. The detector self-calibrates its echo rejection
     *  ([com.axon.voice.audio.BargeDetector]) so there's nothing to tune — this
     *  is the only barge-in control. When false, the mic isn't watched during a
     *  reply at all: playback runs to completion and the user waits for it to
     *  finish (or taps Stop / says "Hey Axon") before speaking again. Read fresh
     *  each reply, so a change takes effect on the next reply with no restart. */
    var bargeInEnabled: Boolean
        get() = sp.getBoolean("barge_in_enabled", true)
        set(v) {
            sp.edit().putBoolean("barge_in_enabled", v).apply()
        }

    // ── Barge-in tuning ──────────────────────────────────────────────────────
    // Optional per-device/room adjustments layered on the self-calibrating
    // detector. Defaults equal BargeDetector's own constants, so an untouched
    // install behaves exactly as before. All are read fresh at the start of each
    // reply's barge monitor (no restart needed) and clamped to safe ranges.

    /** Echo-rejection margin: how far over the learned echo the mic must read to
     *  start ducking. Lower = more sensitive (easier to interrupt), higher =
     *  stricter. Default = BargeDetector.MARGIN (2.0). */
    var bargeMargin: Float
        get() = sp.getFloat("barge_margin", 2.0f).coerceIn(1.2f, 3.0f)
        set(v) { sp.edit().putFloat("barge_margin", v).apply() }

    /** Speech-shape gate ceiling (spectral flatness + zero-crossing rate) that
     *  rejects loud non-speech bursts (coughs, claps, pops). Lower = stricter
     *  (more is filtered out). Default = BargeDetector.FLATNESS_MAX (0.35). */
    var bargeSpeechThreshold: Float
        get() = sp.getFloat("barge_speech_threshold", 0.35f).coerceIn(0.20f, 0.50f)
        set(v) { sp.edit().putFloat("barge_speech_threshold", v).apply() }

    /** Interrupt hold: consecutive ~100ms speech ticks required to confirm a
     *  barge-in. Higher = you must keep talking longer to interrupt. Default =
     *  BargeDetector.MIN_ONSET_TICKS (3 ≈ 300ms). */
    var bargeOnsetTicks: Int
        get() = sp.getInt("barge_onset_ticks", 3).coerceIn(1, 8)
        set(v) { sp.edit().putInt("barge_onset_ticks", v).apply() }

    /** Follow-up window: ~100ms ticks the mic stays open after a spoken reply
     *  before the hands-free exchange ends. Higher = longer to start answering.
     *  Default = SilenceWatcher.NO_SPEECH_TICKS (50 ≈ 5s). */
    var followupWindowTicks: Int
        get() = sp.getInt("followup_window_ticks", 50).coerceIn(30, 150)
        set(v) { sp.edit().putInt("followup_window_ticks", v).apply() }

    /** The chat thread id, shared by the Chat page and the "Hey Axon" wake
     *  service so hands-free exchanges land in the same conversation (history,
     *  agent context, dashboard thread) as typed and push-to-talk messages.
     *  Rotate via [newSession] to start the next conversation. */
    val chatSessionId: String get() = sessionFor("chat")

    private fun sessionFor(surface: String): String {
        sp.getString("session_id_$surface", null)?.let { return it }
        return newSession(surface)
    }

    /** Mint and persist a fresh id for [surface] — the next message opens a
     *  brand-new conversation thread. */
    fun newSession(surface: String): String {
        val id = "$surface-" + UUID.randomUUID().toString().take(8)
        sp.edit().putString("session_id_$surface", id).apply()
        return id
    }

    /** Mint a brand-new id for one hands-free ("Hey Axon") conversation.
     *  Unlike [newSession] this is deliberately NOT persisted as a surface's
     *  shared session: every wake starts its own conversation — saved on its
     *  own (server thread + local [com.axon.voice.ui.ChatHistory] file) and
     *  reviewable in the dashboard chat history — and the next wake starts
     *  another. Follow-ups and a mid-reply "Hey Axon" barge-in stay inside the
     *  same id (the wake service mints it once per interaction, not per turn). */
    fun newWakeConversationId(): String = "wake-" + UUID.randomUUID().toString().take(8)

    val configured: Boolean
        get() = baseUrl.isNotEmpty() && masterKey.isNotEmpty()

    /** ws(s)://host/ws?api_key=… — the WS upgrade can't carry headers from OkHttp
     *  to axum's extractor, so auth rides the query string like the dashboard. */
    fun wsUrl(): String {
        val ws = baseUrl.replaceFirst("http", "ws")
        return "$ws/ws?api_key=" + URLEncoder.encode(masterKey, "UTF-8")
    }
}
