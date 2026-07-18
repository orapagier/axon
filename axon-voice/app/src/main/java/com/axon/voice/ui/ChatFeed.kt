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
        fun onMessage(role: String, text: String)
    }

    /** Set by [ChatActivity] for its lifetime; invoked from service threads. */
    @Volatile
    var listener: Listener? = null

    fun post(ctx: Context, sessionId: String, role: String, text: String) {
        if (text.isBlank()) return
        ChatHistory.append(ctx, sessionId, TranscriptAdapter.Msg(role, text))
        listener?.onMessage(role, text)
    }
}
