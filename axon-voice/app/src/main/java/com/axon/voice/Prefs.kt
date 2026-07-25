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

    // ── Follow-up window ─────────────────────────────────────────────────────

    /** Follow-up window: ~100ms ticks the mic stays open after a spoken reply
     *  before the hands-free exchange ends. Higher = longer to start answering.
     *  Default = SilenceWatcher.NO_SPEECH_TICKS (50 ≈ 5s). */
    var followupWindowTicks: Int
        get() = sp.getInt("followup_window_ticks", 50).coerceIn(30, 150)
        set(v) { sp.edit().putInt("followup_window_ticks", v).apply() }

    // ── Barge-in (talk over a reply) ─────────────────────────────────────────

    /** Whether the reply listens for a spoken interruption while it plays. Off
     *  by default: the reply plays to completion and the user waits or taps
     *  Stop. On, a low-band [com.axon.voice.audio.BandGate] watches for the
     *  user's voice under the (higher-pitched) TTS voice and, on a real spoken
     *  interruption, cuts the reply short and treats the words as the next turn.
     *  See [[barge-in-speaker-verification]] for why this is band-based, not AEC. */
    var bargeInEnabled: Boolean
        get() = sp.getBoolean("barge_in_enabled", false)
        set(v) { sp.edit().putBoolean("barge_in_enabled", v).apply() }

    /** Barge trigger level, stored as RMS×10000 so it reads as an integer on a
     *  slider. The filtered low-band RMS must exceed this for the gate to fire.
     *  Tuned on-device against the live `band=` value in the notification: set
     *  it just above the idle/echo level so the reply's own voice never trips
     *  it but yours does. Default 150 (0.0150). */
    var bargeBandThreshold: Int
        get() = sp.getInt("barge_band_threshold", 150).coerceIn(10, 1000)
        set(v) { sp.edit().putInt("barge_band_threshold", v).apply() }

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
     *  another. Follow-up turns stay inside the same id (the wake service
     *  mints it once per interaction, not per turn). */
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
