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

    /** Per-surface chat thread ids. The Chat page and the "Hey Axon" wake
     *  service share [chatSessionId] so hands-free exchanges land in the same
     *  conversation (history, agent context, dashboard thread) as typed and
     *  push-to-talk chat messages; the voice orb keeps its own thread. Rotate
     *  one via [newSession] to start that surface's next conversation. */
    val chatSessionId: String get() = sessionFor("chat")
    val voiceSessionId: String get() = sessionFor("voice")

    private fun sessionFor(surface: String): String {
        sp.getString("session_id_$surface", null)?.let { return it }
        // Pre-split installs stored one id (shared by UI + wake service) under
        // "session_id"; keep it as the voice thread so that conversation's
        // server-side context carries over instead of silently resetting.
        if (surface == "voice") {
            sp.getString("session_id", null)?.let {
                sp.edit().putString("session_id_voice", it).apply()
                return it
            }
        }
        return newSession(surface)
    }

    /** Mint and persist a fresh id for [surface] ("chat"|"voice"|"wake") —
     *  the next message opens a brand-new conversation thread. */
    fun newSession(surface: String): String {
        val id = "$surface-" + UUID.randomUUID().toString().take(8)
        sp.edit().putString("session_id_$surface", id).apply()
        return id
    }

    val configured: Boolean
        get() = baseUrl.isNotEmpty() && masterKey.isNotEmpty()

    /** ws(s)://host/ws?api_key=… — the WS upgrade can't carry headers from OkHttp
     *  to axum's extractor, so auth rides the query string like the dashboard. */
    fun wsUrl(): String {
        val ws = baseUrl.replaceFirst("http", "ws")
        return "$ws/ws?api_key=" + URLEncoder.encode(masterKey, "UTF-8")
    }
}
