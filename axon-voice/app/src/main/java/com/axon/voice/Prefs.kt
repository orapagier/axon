package com.axon.voice

import android.content.Context
import com.axon.voice.audio.BargeDetector
import com.axon.voice.audio.SPEAKER_SIMILARITY_THRESHOLD
import com.axon.voice.audio.TtsPlayer
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

    /** Cosine-similarity cutoff a mid-reply interruption's voice must clear
     *  against the enrolled voiceprint before it counts as a real barge-in
     *  (see [com.axon.voice.audio.speakerVerifier]). Lower = easier for your
     *  own voice to interrupt (but lets more of other people's through);
     *  higher = stricter. User-tweakable in Settings because the right value
     *  depends on mic/room/voice and the [SPEAKER_SIMILARITY_THRESHOLD]
     *  default was never calibrated on real hardware. Read fresh at the start
     *  of every reply's barge monitor, so a change takes effect on the next
     *  reply with no restart. Only has any effect while a voiceprint is
     *  enrolled — barge-in is energy-only otherwise. */
    var bargeMatchThreshold: Float
        get() = sp.getFloat("barge_match_threshold", SPEAKER_SIMILARITY_THRESHOLD)
        set(v) {
            sp.edit().putFloat("barge_match_threshold", v).apply()
        }

    // ── Barge-in energy/echo tuning ───────────────────────────────────────────
    // The four knobs below feed the energy gate that runs *before* the speaker
    // match ([bargeMatchThreshold]) — a mid-reply interruption has to clear this
    // gate first, so if it's mistuned the voice match never even gets consulted.
    // All read fresh at the start of every reply (BargeDetector.tune /
    // TtsPlayer.duckLevel), so a slider change lands on the next reply with no
    // restart. Defaults are the code defaults; [resetBargeTuning] restores them.

    /** How far over the learned echo level the mic must read to count as a real
     *  interruption rather than the reply bouncing back ([BargeDetector.MARGIN]).
     *  Lower = easier to interrupt (more sensitive); higher = fewer false
     *  triggers but a harder barge-in. The primary lever when your own voice
     *  can't get through on the energy side. */
    var bargeMargin: Float
        get() = sp.getFloat("barge_margin", BargeDetector.MARGIN.toFloat())
        set(v) {
            sp.edit().putFloat("barge_margin", v).apply()
        }

    /** How hard a false alarm bumps the learned echo gain to stop the reply's
     *  volume "pumping" ([BargeDetector.FALSE_ALARM_GAIN_BOOST]). 1.0 turns it
     *  off. Higher fights pumping harder but raises the barge-in threshold,
     *  which can start swallowing real interruptions — the direct trade-off
     *  between the "won't recognize my voice" and "volume pumps" symptoms. */
    var bargeEchoBoost: Float
        get() = sp.getFloat("barge_echo_boost", BargeDetector.FALSE_ALARM_GAIN_BOOST.toFloat())
        set(v) {
            sp.edit().putFloat("barge_echo_boost", v).apply()
        }

    /** How far the reply is ducked while checking a tentative interruption
     *  ([TtsPlayer.DEFAULT_DUCK_VOLUME], 0..1). Lower makes the mic hear past
     *  the echo more easily (better barge-in detection) but the volume dip is
     *  more noticeable on a false alarm. */
    var bargeDuckVolume: Float
        get() = sp.getFloat("barge_duck_volume", TtsPlayer.DEFAULT_DUCK_VOLUME)
        set(v) {
            sp.edit().putFloat("barge_duck_volume", v).apply()
        }

    /** Per-tick peak-hold decay on the echo reference
     *  ([BargeDetector.PLAYREF_DECAY], 0..1). Higher holds the reference longer
     *  across the gaps between streamed sentences (less pumping at sentence
     *  boundaries); too high and a genuinely finished reply's reference lingers. */
    var bargePlayrefDecay: Float
        get() = sp.getFloat("barge_playref_decay", BargeDetector.PLAYREF_DECAY.toFloat())
        set(v) {
            sp.edit().putFloat("barge_playref_decay", v).apply()
        }

    /** Restore every barge-in tuning knob (match + the four energy/echo knobs)
     *  to its code default — the escape hatch after over-tweaking. */
    fun resetBargeTuning() {
        sp.edit()
            .remove("barge_match_threshold")
            .remove("barge_margin")
            .remove("barge_echo_boost")
            .remove("barge_duck_volume")
            .remove("barge_playref_decay")
            .apply()
    }

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
