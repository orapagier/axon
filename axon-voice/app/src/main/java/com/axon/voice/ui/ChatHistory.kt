package com.axon.voice.ui

import android.content.Context
import org.json.JSONArray
import org.json.JSONObject
import java.io.File

/**
 * Local persistence for the Chat page transcript: one JSON file per chat
 * session id under filesDir, capped at [MAX_MESSAGES], so reopening the page
 * (or the app) shows the conversation where it left off. Wake-word exchanges
 * are appended here too (via [ChatFeed]); only the voice orb's transcript
 * stays ephemeral — its record of truth is the dashboard conversation thread.
 *
 * Methods are synchronized because the Chat page (snapshot saves) and the
 * wake service ([append]) write the same file from different threads.
 */
object ChatHistory {
    private const val MAX_MESSAGES = 200

    private fun file(ctx: Context, sessionId: String) =
        File(ctx.filesDir, "chat_$sessionId.json")

    /** Add one message to the saved transcript without disturbing a concurrent
     *  snapshot save from the Chat page. */
    @Synchronized
    fun append(ctx: Context, sessionId: String, msg: TranscriptAdapter.Msg) {
        save(ctx, sessionId, load(ctx, sessionId) + msg)
    }

    @Synchronized
    fun load(ctx: Context, sessionId: String): List<TranscriptAdapter.Msg> = runCatching {
        val f = file(ctx, sessionId)
        if (!f.exists()) return emptyList()
        val arr = JSONArray(f.readText())
        (0 until arr.length()).mapNotNull { i ->
            val o = arr.optJSONObject(i) ?: return@mapNotNull null
            val text = o.optString("text", "")
            if (text.isEmpty()) null
            else TranscriptAdapter.Msg(o.optString("role", "assistant"), text)
        }
    }.getOrDefault(emptyList())

    @Synchronized
    fun save(ctx: Context, sessionId: String, msgs: List<TranscriptAdapter.Msg>) {
        runCatching {
            val arr = JSONArray()
            for (m in msgs.takeLast(MAX_MESSAGES)) {
                arr.put(JSONObject().put("role", m.role).put("text", m.text))
            }
            file(ctx, sessionId).writeText(arr.toString())
        }
    }

    @Synchronized
    fun delete(ctx: Context, sessionId: String) {
        file(ctx, sessionId).delete()
    }

    /** One saved "Hey Axon" conversation, as shown in the History list: enough
     *  to identify and preview it without the caller loading the full
     *  transcript twice. [updatedAt] is the file's last-write time — good
     *  enough for "most recent first" without a separate timestamp in the
     *  JSON itself. */
    data class Summary(val sessionId: String, val preview: String, val updatedAt: Long)

    /** Every saved wake conversation ([Prefs.newWakeConversationId] mints one
     *  id per "Hey Axon", distinct from the single ongoing
     *  [Prefs.chatSessionId] thread the Chat page already shows), newest
     *  first. The manual chat thread is deliberately excluded — it's always
     *  one tap away by just opening the app, where this list surfaces the
     *  hands-free exchanges that otherwise have no other way back to them. */
    @Synchronized
    fun listWakeConversations(ctx: Context): List<Summary> {
        val files = ctx.filesDir.listFiles { f -> f.name.startsWith("chat_wake-") && f.name.endsWith(".json") }
            ?: return emptyList()
        return files.mapNotNull { f ->
            val sessionId = f.name.removePrefix("chat_").removeSuffix(".json")
            val msgs = load(ctx, sessionId)
            if (msgs.isEmpty()) return@mapNotNull null
            val preview = msgs.firstOrNull { it.role == "user" }?.text ?: msgs.first().text
            Summary(sessionId, preview, f.lastModified())
        }.sortedByDescending { it.updatedAt }
    }
}
