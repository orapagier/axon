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
import com.google.android.material.materialswitch.MaterialSwitch
import org.json.JSONObject
import kotlin.concurrent.thread

/**
 * Typed-chat surface: compose by keyboard, or tap the mic to dictate into the
 * input (recorded with the same silence watcher as push-to-talk, transcribed
 * server-side, then left in the field for editing before send). Replies stream
 * in as text and are never auto-spoken — the orb screen is the spoken surface.
 *
 * Runs on its own session id ([Prefs.chatSessionId]) so this thread's history
 * and agent context never blend with the voice orb or wake-word conversations;
 * the transcript is persisted locally per session via [ChatHistory]. The
 * "Hey Axon" toggle is mirrored here so hands-free can be flipped without
 * leaving the page.
 */
class ChatActivity : AppCompatActivity(), ChatSocket.Listener {

    private enum class State { IDLE, RECORDING, TRANSCRIBING, WAITING }

    private lateinit var prefs: Prefs
    private lateinit var client: AxonClient
    private var chat: ChatSocket? = null

    private lateinit var connLabel: TextView
    private lateinit var wakeSwitch: MaterialSwitch
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

    private val permLauncher =
        registerForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { grants ->
            if (grants[Manifest.permission.RECORD_AUDIO] == true && pendingDictate) {
                pendingDictate = false
                startDictation()
            }
        }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_chat)

        prefs = Prefs(this)
        client = AxonClient(prefs)

        connLabel = findViewById(R.id.connLabel)
        wakeSwitch = findViewById(R.id.wakeSwitch)
        input = findViewById(R.id.chatInput)
        micBtn = findViewById(R.id.micBtn)
        sendBtn = findViewById(R.id.sendBtn)
        list = findViewById(R.id.chatList)
        list.layoutManager = LinearLayoutManager(this).apply { stackFromEnd = true }
        list.adapter = adapter

        adapter.load(ChatHistory.load(this, prefs.chatSessionId))
        scrollEnd()

        findViewById<ImageButton>(R.id.backBtn).setOnClickListener { finish() }
        findViewById<ImageButton>(R.id.newChatBtn).setOnClickListener { newConversation() }
        micBtn.setOnClickListener { onMicTap() }
        sendBtn.setOnClickListener { onSendTap() }

        wakeSwitch.setOnCheckedChangeListener { _, checked ->
            if (checked == WakeWordService.running) return@setOnCheckedChangeListener
            if (checked) {
                if (!hasMicPermission()) {
                    wakeSwitch.isChecked = false
                    requestPerms()
                    return@setOnCheckedChangeListener
                }
                prefs.wakeEnabled = true
                WakeWordService.start(this)
                requestBatteryExemption()
            } else {
                prefs.wakeEnabled = false
                WakeWordService.stop(this)
            }
        }
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
        wakeSwitch.isChecked = WakeWordService.running
    }

    override fun onStop() {
        ChatHistory.save(this, prefs.chatSessionId, adapter.snapshot())
        super.onStop()
    }

    override fun onDestroy() {
        if (state == State.RECORDING) {
            recorder?.let { runCatching { it.stop() } }
            recorder = null
            WakeWordService.micHold = false
        }
        chat?.close()
        super.onDestroy()
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
                if (text.isNotBlank()) {
                    val existing = input.text.toString()
                    val glue = if (existing.isEmpty() || existing.endsWith(" ")) "" else " "
                    input.append(glue + text)
                    input.setSelection(input.text.length)
                }
                resetInputRow()
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
            ChatHistory.save(this, prefs.chatSessionId, adapter.snapshot())
            return
        }
        if (state != State.IDLE) return
        val text = input.text.toString().trim()
        if (text.isEmpty()) return
        input.setText("")
        adapter.add("user", text)
        adapter.add("assistant", "")
        scrollEnd()
        state = State.WAITING
        ChatHistory.save(this, prefs.chatSessionId, adapter.snapshot())
        if (chat?.sendTask(text, prefs.chatSessionId) != true) {
            adapter.setLast(getString(R.string.status_offline))
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
                ChatHistory.save(this, prefs.chatSessionId, adapter.snapshot())
            }
        }
    }

    override fun onWsEvent(ev: JSONObject) {
        main.post {
            when (ev.optString("type")) {
                "token" -> if (state == State.WAITING && adapter.lastRole == "assistant") {
                    adapter.appendToLast(ev.optString("text"))
                    scrollEnd()
                }

                "done" -> if (state == State.WAITING) {
                    val full = ev.optString("full_text", "")
                    if (adapter.lastRole == "assistant" && adapter.lastText.isBlank() && full.isNotBlank()) {
                        adapter.setLast(full)
                    }
                    scrollEnd()
                    state = State.IDLE
                    ChatHistory.save(this, prefs.chatSessionId, adapter.snapshot())
                }

                "error" -> if (state == State.WAITING) {
                    adapter.add("error", ev.optString("message", "something went wrong"))
                    scrollEnd()
                    state = State.IDLE
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
        }
        // RECORDING/TRANSCRIBING are left to finish on their own — dictation
        // lands in the input field, which survives the thread switch.
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
