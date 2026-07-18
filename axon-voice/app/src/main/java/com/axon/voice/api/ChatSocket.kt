package com.axon.voice.api

import android.os.Handler
import android.os.Looper
import com.axon.voice.Prefs
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import org.json.JSONObject

/**
 * The dashboard chat protocol over /ws: send {task, session_id}, receive
 * AgentEvent frames ({"type": "token"|"thinking"|"done"|"error"|…}).
 * Auto-reconnects every 3s while open, mirroring axon-ui/src/lib/ws.js.
 */
class ChatSocket(
    private val prefs: Prefs,
    private val http: OkHttpClient,
    private val listener: Listener,
) {
    interface Listener {
        fun onWsConnected() {}
        fun onWsDisconnected() {}
        fun onWsEvent(ev: JSONObject)
    }

    @Volatile
    private var ws: WebSocket? = null

    @Volatile
    private var wantOpen = false

    @Volatile
    var connected = false
        private set

    private val main = Handler(Looper.getMainLooper())

    fun open() {
        if (wantOpen) return
        wantOpen = true
        connect()
    }

    private fun connect() {
        if (!prefs.configured) return
        val req = Request.Builder().url(prefs.wsUrl()).build()
        ws = http.newWebSocket(req, object : WebSocketListener() {
            override fun onOpen(webSocket: WebSocket, response: Response) {
                if (ws !== webSocket) return
                connected = true
                listener.onWsConnected()
            }

            override fun onMessage(webSocket: WebSocket, text: String) {
                if (ws !== webSocket) return
                runCatching { JSONObject(text) }.getOrNull()?.let { listener.onWsEvent(it) }
            }

            override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                dropped(webSocket)
            }

            override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                dropped(webSocket)
            }

            private fun dropped(webSocket: WebSocket) {
                if (ws !== webSocket) return
                connected = false
                listener.onWsDisconnected()
                if (wantOpen) {
                    main.postDelayed({ if (wantOpen) connect() }, 3000)
                }
            }
        })
    }

    fun sendTask(task: String, sessionId: String): Boolean {
        val payload = JSONObject().put("task", task).put("session_id", sessionId)
        return ws?.send(payload.toString()) == true
    }

    fun cancel(sessionId: String) {
        val payload = JSONObject().put("type", "cancel").put("session_id", sessionId)
        ws?.send(payload.toString())
    }

    fun close() {
        wantOpen = false
        connected = false
        ws?.close(1000, null)
        ws = null
    }
}
