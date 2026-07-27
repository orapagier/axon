package com.axon.androidcompanion

import android.content.Context
import java.io.File
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

    // ── Wake-word enrollment ─────────────────────────────────────────────────

    /** The phrase the user enrolled during onboarding (e.g. "Hey Axon", or
     *  whatever they chose). Empty until enrollment completes; UI/prompts
     *  should fall back to the "Hey Axon" default label when empty. */
    var wakePhrase: String
        get() = sp.getString("wake_phrase", "") ?: ""
        set(v) { sp.edit().putString("wake_phrase", v.trim()).apply() }

    /** Absolute path to the on-device-built personal .rpw model
     *  (filesDir/wake_model.rpw). Empty until enrollment completes — there is
     *  no bundled fallback model, so [wakeEnrolled] gates ever starting
     *  [WakeWordService]. */
    var wakeModelPath: String
        get() = sp.getString("wake_model_path", "") ?: ""
        set(v) { sp.edit().putString("wake_model_path", v).apply() }

    /** True once a personal model has been built and its file still exists. */
    val wakeEnrolled: Boolean
        get() = wakeModelPath.isNotEmpty() && File(wakeModelPath).exists()

    // ── Follow-up window ─────────────────────────────────────────────────────

    /** Follow-up window: ~100ms ticks the mic stays open after a spoken reply
     *  before the hands-free exchange ends. Higher = longer to start answering.
     *  Default = SilenceWatcher.NO_SPEECH_TICKS (50 ≈ 5s). */
    var followupWindowTicks: Int
        get() = sp.getInt("followup_window_ticks", 50).coerceIn(30, 150)
        set(v) { sp.edit().putInt("followup_window_ticks", v).apply() }

    // ── Barge-in (talk over a reply) ─────────────────────────────────────────

    /** Whether the reply listens for a spoken interruption while it plays. Off
     *  by default. Only takes effect with a HEADSET connected: the reply then
     *  plays into the earpiece, not the loudspeaker, so there's no echo path
     *  back into the mic and a plain loudness trigger is safe. On the built-in
     *  speaker the reply plays to completion regardless (the echo can't be
     *  separated from the user's voice — see [[barge-in-speaker-verification]]). */
    var bargeInEnabled: Boolean
        get() = sp.getBoolean("barge_in_enabled", false)
        set(v) { sp.edit().putBoolean("barge_in_enabled", v).apply() }

    /** Barge trigger loudness, stored as RMS×10000 so it reads as an integer on
     *  a slider. The mic's broadband RMS must exceed this for a few frames to
     *  interrupt a reply — a raised bar so only near, deliberate speech triggers
     *  and distant/quiet voices don't. Tuned on-device against the live `mic=`
     *  value in the notification. Default 600 (0.0600). */
    var bargeOnsetLevel: Int
        get() = sp.getInt("barge_onset_level", 600).coerceIn(100, 3000)
        set(v) { sp.edit().putInt("barge_onset_level", v).apply() }

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
     *  own (server thread + local [com.axon.androidcompanion.ui.ChatHistory] file) and
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
