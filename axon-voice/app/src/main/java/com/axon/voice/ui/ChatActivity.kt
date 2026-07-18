package com.axon.voice.ui

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.provider.Settings
import android.widget.EditText
import android.widget.ImageButton
import android.widget.TextView
import android.widget.Toast
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.ContextCompat
import androidx.recyclerview.widget.LinearLayoutManager
import androidx.recyclerview.widget.RecyclerView
import com.axon.voice.Prefs
import com.axon.voice.R
import com.axon.voice.api.AxonClient
import com.axon.voice.api.ChatSocket
import com.axon.voice.audio.SilenceWatcher
import com.axon.voice.audio.WavRecorder
import com.axon.voice.wake.WakeWordService
import org.json.JSONObject
import kotlin.concurrent.thread

/**
 * The chat surface: type a message, or tap the mic to speak one — the
 * recording (same silence watcher as the orb's push-to-talk) is transcribed
 * server-side and sent as its own chat message, never left in the composer.
 * Replies stream in as text and are never auto-spoken — the orb screen is the
 * spoken surface.
 *
 * Runs on [Prefs.chatSessionId], which the "Hey Axon" wake service shares:
 * hands-free exchanges arrive through [ChatFeed] and show here (and persist
 * via [ChatHistory]) exactly like typed messages, so the whole conversation —
 * typed, push-to-talk, and hands-free — lives in one saved thread. The wake
 * button in the input row toggles the hands-free listener without leaving the
 * page.
 */
class ChatActivity : AppCompatActivity(), ChatSocket.Listener {

    private enum class State { IDLE, RECORDING, TRANSCRIBING, WAITING }

    private lateinit var prefs: Prefs
    private lateinit var client: AxonClient
    private var chat: ChatSocket? = null

    private lateinit var connLabel: TextView
    private lateinit var wakeBtn: ImageButton
    private lateinit var input: EditText
    private lateinit var micBtn: ImageButton
    private lateinit var sendBtn: ImageButton
    private lateinit var list: RecyclerView
    private val adapter = TranscriptAdapter()
    private val main = Handler(Looper.getMainLooper())

    private var state = State.IDLE
    private var recorder: WavRecorder? = null
    private var watcher: SilenceWatcher? = null
    private var pendingDictate = false
    private var pendingWake = false

    /** Adapter index of the assistant bubble the current run streams into.
     *  Index-addressed (not "last item") because a wake-word exchange can be
     *  appended below it mid-stream via [ChatFeed]. */
    private var streamIdx = -1

    /** Live inserts from the wake service — its exchange is already persisted
     *  by [ChatFeed.post]; this only mirrors it into the open list. */
    private val feedListener = ChatFeed.Listener { role, text ->
        main.post {
            adapter.add(role, text)
            scrollEnd()
        }
    }

    private val permLauncher =
        registerForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { grants ->
            if (grants[Manifest.permission.RECORD_AUDIO] == true) {
                if (pendingDictate) {
                    pendingDictate = false
                    startDictation()
                }
                if (pendingWake) {
                    pendingWake = false
                    setWakeEnabled(true)
                }
            }
        }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_chat)

        prefs = Prefs(this)
        client = AxonClient(prefs)

        connLabel = findViewById(R.id.connLabel)
        wakeBtn = findViewById(R.id.wakeBtn)
        input = findViewById(R.id.chatInput)
        micBtn = findViewById(R.id.micBtn)
        sendBtn = findViewById(R.id.sendBtn)
        list = findViewById(R.id.chatList)
        list.layoutManager = LinearLayoutManager(this).apply { stackFromEnd = true }
        list.adapter = adapter

        adapter.load(ChatHistory.load(this, prefs.chatSessionId))
        scrollEnd()
        ChatFeed.listener = feedListener

        findViewById<ImageButton>(R.id.backBtn).setOnClickListener { finish() }
        findViewById<ImageButton>(R.id.newChatBtn).setOnClickListener { newConversation() }
        micBtn.setOnClickListener { onMicTap() }
        sendBtn.setOnClickListener { onSendTap() }
        wakeBtn.setOnClickListener { setWakeEnabled(!WakeWordService.running) }
    }

    override fun onStart() {
        super.onStart()
        if (!prefs.configured) {
            startActivity(Intent(this, SettingsActivity::class.java))
            return
        }
        if (chat == null || chat?.connected != true) {
            chat?.close()
            chat = ChatSocket(prefs, client.http, this).also { it.open() }
        }
    }

    override fun onResume() {
        super.onResume()
        updateWakeBtn()
    }

    override fun onStop() {
        ChatHistory.save(this, prefs.chatSessionId, adapter.snapshot())
        super.onStop()
    }

    override fun onDestroy() {
        if (ChatFeed.listener === feedListener) ChatFeed.listener = null
        if (state == State.RECORDING) {
            recorder?.let { runCatching { it.stop() } }
            recorder = null
            WakeWordService.micHold = false
        }
        chat?.close()
        super.onDestroy()
    }

    // ── Wake word ("Hey Axon") toggle ───────────────────────────────────────

    private fun setWakeEnabled(on: Boolean) {
        if (on == WakeWordService.running) {
            updateWakeBtn()
            return
        }
        if (on) {
            if (!hasMicPermission()) {
                pendingWake = true
                requestPerms()
                return
            }
            prefs.wakeEnabled = true
            WakeWordService.start(this)
            requestBatteryExemption()
        } else {
            prefs.wakeEnabled = false
            WakeWordService.stop(this)
        }
        // The service flips `running` asynchronously — reflect the intent now.
        updateWakeBtn(on)
    }

    private fun updateWakeBtn(active: Boolean = WakeWordService.running) {
        wakeBtn.setColorFilter(
            ContextCompat.getColor(this, if (active) R.color.accent else R.color.text_dim)
        )
    }

    // ── Dictation ───────────────────────────────────────────────────────────

    private fun onMicTap() {
        when (state) {
            State.IDLE -> startDictation()
            State.RECORDING -> stopDictation()
            else -> {} // busy transcribing or waiting on a reply
        }
    }

    private fun startDictation() {
        if (!prefs.configured) {
            startActivity(Intent(this, SettingsActivity::class.java))
            return
        }
        if (!hasMicPermission()) {
            pendingDictate = true
            requestPerms()
            return
        }
        if (state != State.IDLE) return
        state = State.RECORDING
        micBtn.setColorFilter(ContextCompat.getColor(this, R.color.error))
        input.hint = getString(R.string.chat_hint_listening)

        val w = SilenceWatcher()
        watcher = w
        val r = WavRecorder()
        recorder = r
        val serviceWasListening = WakeWordService.running
        WakeWordService.micHold = true
        thread(name = "axon-dictate-start") {
            // Give the wake service a beat to release the shared microphone.
            if (serviceWasListening) Thread.sleep(300)
            try {
                r.start { rms ->
                    if (w.tick(rms)) main.post { stopDictation() }
                }
            } catch (e: Exception) {
                WakeWordService.micHold = false
                main.post {
                    toastMsg(e.message ?: "microphone unavailable")
                    resetInputRow()
                }
            }
        }
    }

    private fun stopDictation() {
        if (state != State.RECORDING) return
        state = State.TRANSCRIBING
        micBtn.clearColorFilter()
        micBtn.alpha = 0.4f
        input.hint = getString(R.string.transcribing)

        val r = recorder
        val w = watcher
        recorder = null
        watcher = null
        thread(name = "axon-dictate-send") {
            val wav = r?.stop()
            WakeWordService.micHold = false
            if (wav == null || w?.hadSpeech != true) {
                main.post { resetInputRow() }
                return@thread
            }
            val text = runCatching { client.transcribe(wav) }.getOrElse { e ->
                main.post {
                    toastMsg(e.message ?: "transcription failed")
                    resetInputRow()
                }
                return@thread
            }
            main.post {
                // Speak-and-go: the transcript sends as its own chat message,
                // never through the composer — a typed draft stays untouched.
                resetInputRow()
                if (text.isNotBlank()) sendMessage(text)
            }
        }
    }

    /** Back to the composable state after dictation ends, however it ended. */
    private fun resetInputRow() {
        if (state == State.RECORDING || state == State.TRANSCRIBING) state = State.IDLE
        micBtn.clearColorFilter()
        micBtn.alpha = 1f
        input.hint = getString(R.string.chat_hint)
    }

    // ── Sending & streaming replies ─────────────────────────────────────────

    private fun onSendTap() {
        if (state == State.WAITING) {
            // Acts as "stop": cancel the in-flight run, keep what streamed.
            chat?.cancel(prefs.chatSessionId)
            state = State.IDLE
            streamIdx = -1
            ChatHistory.save(this, prefs.chatSessionId, adapter.snapshot())
            return
        }
        if (state != State.IDLE) return
        val text = input.text.toString().trim()
        if (text.isEmpty()) return
        input.setText("")
        sendMessage(text)
    }

    /** The one path into a run for typed and push-to-talk messages alike: show
     *  the user bubble, open a streaming assistant bubble, ship the task. */
    private fun sendMessage(text: String) {
        if (state != State.IDLE) return
        adapter.add("user", text)
        adapter.add("assistant", "")
        streamIdx = adapter.lastIndex
        scrollEnd()
        state = State.WAITING
        ChatHistory.save(this, prefs.chatSessionId, adapter.snapshot())
        if (chat?.sendTask(text, prefs.chatSessionId) != true) {
            adapter.setAt(streamIdx, getString(R.string.status_offline))
            streamIdx = -1
            state = State.IDLE
        }
    }

    override fun onWsConnected() {
        main.post { connLabel.text = "online" }
    }

    override fun onWsDisconnected() {
        main.post {
            connLabel.text = "offline"
            if (state == State.WAITING) {
                // The run may still finish server-side; its result lands in the
                // dashboard thread. Unblock the composer rather than hanging.
                state = State.IDLE
                streamIdx = -1
                ChatHistory.save(this, prefs.chatSessionId, adapter.snapshot())
            }
        }
    }

    override fun onWsEvent(ev: JSONObject) {
        main.post {
            when (ev.optString("type")) {
                "token" -> if (state == State.WAITING && streamIdx >= 0) {
                    adapter.appendAt(streamIdx, ev.optString("text"))
                    scrollEnd()
                }

                "done" -> if (state == State.WAITING) {
                    val full = ev.optString("full_text", "")
                    if (streamIdx >= 0 && adapter.textAt(streamIdx).isBlank() && full.isNotBlank()) {
                        adapter.setAt(streamIdx, full)
                    }
                    scrollEnd()
                    state = State.IDLE
                    streamIdx = -1
                    ChatHistory.save(this, prefs.chatSessionId, adapter.snapshot())
                }

                "error" -> if (state == State.WAITING) {
                    adapter.add("error", ev.optString("message", "something went wrong"))
                    scrollEnd()
                    state = State.IDLE
                    streamIdx = -1
                    ChatHistory.save(this, prefs.chatSessionId, adapter.snapshot())
                }
            }
        }
    }

    // ── New conversation ────────────────────────────────────────────────────

    private fun newConversation() {
        if (state == State.WAITING) {
            chat?.cancel(prefs.chatSessionId)
            state = State.IDLE
            streamIdx = -1
        }
        // RECORDING/TRANSCRIBING are left to finish on their own — an
        // in-flight transcription simply sends into the fresh thread. The wake
        // service reads the session id per exchange, so it follows along too.
        ChatHistory.delete(this, prefs.chatSessionId)
        prefs.newSession("chat")
        adapter.clear()
        toastMsg(getString(R.string.new_conversation_started))
    }

    // ── Permissions & housekeeping ──────────────────────────────────────────

    private fun hasMicPermission(): Boolean =
        ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO) ==
            PackageManager.PERMISSION_GRANTED

    private fun requestPerms() {
        val wanted = mutableListOf(Manifest.permission.RECORD_AUDIO)
        if (Build.VERSION.SDK_INT >= 33) {
            wanted.add(Manifest.permission.POST_NOTIFICATIONS)
        }
        permLauncher.launch(wanted.toTypedArray())
    }

    private fun requestBatteryExemption() {
        val pm = getSystemService(PowerManager::class.java)
        if (!pm.isIgnoringBatteryOptimizations(packageName)) {
            runCatching {
                startActivity(
                    Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS)
                        .setData(Uri.parse("package:$packageName"))
                )
            }
        }
    }

    private fun toastMsg(msg: String) {
        Toast.makeText(this, msg, Toast.LENGTH_SHORT).show()
    }

    private fun scrollEnd() {
        if (adapter.itemCount > 0) list.scrollToPosition(adapter.itemCount - 1)
    }
}
