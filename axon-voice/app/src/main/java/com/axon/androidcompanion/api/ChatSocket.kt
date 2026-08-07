package com.axon.androidcompanion.api

import android.os.Handler
import android.os.Looper
import com.axon.androidcompanion.Prefs
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import org.json.JSONObject
import java.util.concurrent.atomic.AtomicInteger

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
    companion object {
        private const val RETRY_MS = 3000L
    }

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

    /** Bumped by every [connect] and by [close]; each listener captures the
     *  value its socket was created under and ignores callbacks once it no
     *  longer matches. This replaces an `ws !== webSocket` identity check,
     *  which compared against a field assigned only *after* newWebSocket()
     *  returns — OkHttp can deliver a callback before that assignment lands, and
     *  the check then swallowed the drop, leaving no reconnect scheduled and the
     *  socket dead for the rest of the process's life. */
    private val gen = AtomicInteger(0)

    private val main = Handler(Looper.getMainLooper())

    fun open() {
        if (wantOpen) return
        wantOpen = true
        connect()
    }

    /** Try to reconnect immediately instead of waiting out the retry timer.
     *  The wake service calls this when it actually needs the socket (a spoken
     *  command is ready to send), so a drop that happened while nobody was
     *  talking costs the user a moment rather than a whole turn. No-op when
     *  already connected or closed for good. */
    fun reconnectNow() {
        if (!wantOpen || connected) return
        main.post { if (wantOpen && !connected) connect() }
    }

    private fun connect() {
        if (!prefs.configured) return
        val myGen = gen.incrementAndGet()
        // Any previous socket is superseded from here: cancel it outright so a
        // half-open one can't keep accepting sends that go nowhere.
        ws?.cancel()
        connected = false
        val req = Request.Builder().url(prefs.wsUrl()).build()
        ws = http.newWebSocket(req, object : WebSocketListener() {

            /** One drop report per socket: [onClosing] completes the handshake,
             *  which makes OkHttp deliver [onClosed] for the same death. */
            private var reported = false

            override fun onOpen(webSocket: WebSocket, response: Response) {
                if (gen.get() != myGen) return
                connected = true
                listener.onWsConnected()
            }

            override fun onMessage(webSocket: WebSocket, text: String) {
                if (gen.get() != myGen) return
                runCatching { JSONObject(text) }.getOrNull()?.let { listener.onWsEvent(it) }
            }

            /**
             * The peer started the closing handshake — the agent restarting, or
             * a proxy/tunnel in front of it hitting an idle timeout.
             *
             * This override is not optional. OkHttp only delivers [onClosed] for
             * a close the *client* enqueued, and its reader loop returns
             * normally after a peer close, so [onFailure] never fires either.
             * Without handling it here a server-initiated close was completely
             * invisible: [connected] stayed true, no reconnect was ever
             * scheduled, and a reply the wake service was waiting on simply
             * never arrived — it sat in "thinking" until its 310s backstop and
             * every later request went into the dead socket.
             */
            override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
                runCatching { webSocket.close(1000, null) }
                dropped()
            }

            override fun onClosed(webSocket: WebSocket, code: Int, reason: String) = dropped()

            override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) =
                dropped()

            private fun dropped() {
                if (reported) return
                reported = true
                if (gen.get() != myGen) return
                connected = false
                listener.onWsDisconnected()
                if (wantOpen) {
                    main.postDelayed({ if (wantOpen && gen.get() == myGen) connect() }, RETRY_MS)
                }
            }
        })
    }

    /** [voice] true when the message was spoken and its reply will be read
     *  aloud — the server answers with a short spoken summary instead of a raw
     *  dump. Wake and push-to-talk pass true; typed chat passes false.
     *
     *  False means the task did not go out. Only a socket we have seen open is
     *  used: OkHttp queues frames onto a connecting or half-dead one and still
     *  returns true, which left callers waiting on a reply that could never
     *  arrive instead of failing fast. */
    fun sendTask(task: String, sessionId: String, voice: Boolean = false): Boolean {
        if (!connected) return false
        val payload = JSONObject()
            .put("task", task)
            .put("session_id", sessionId)
            .put("voice", voice)
        return ws?.send(payload.toString()) == true
    }

    fun cancel(sessionId: String) {
        if (!connected) return
        val payload = JSONObject().put("type", "cancel").put("session_id", sessionId)
        ws?.send(payload.toString())
    }

    fun close() {
        wantOpen = false
        connected = false
        gen.incrementAndGet() // silence the live listener and its pending retry
        ws?.close(1000, null)
        ws = null
    }
}
