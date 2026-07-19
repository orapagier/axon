package com.axon.voice.ui

import android.content.Context

/**
 * Bridge from the wake-word service into the Chat page: every hands-free
 * exchange is appended to the chat session's saved transcript (so it reads
 * back — links and all — like any typed conversation) and, while the page
 * exists, mirrored into its live list through [listener].
 */
object ChatFeed {
    fun interface Listener {
        fun onMessage(sessionId: String, role: String, text: String)
    }

    /** Set by [ChatActivity] for its lifetime; invoked from service threads. */
    @Volatile
    var listener: Listener? = null

    fun post(ctx: Context, sessionId: String, role: String, text: String) {
        if (text.isBlank()) return
        ChatHistory.append(ctx, sessionId, TranscriptAdapter.Msg(role, text))
        // [sessionId] rides along so the open page can ignore exchanges that
        // belong to a different conversation — each "Hey Axon" runs in its own
        // wake conversation, which must not be interleaved into (or saved
        // under) the manual chat thread the page is showing.
        listener?.onMessage(sessionId, role, text)
    }
}
