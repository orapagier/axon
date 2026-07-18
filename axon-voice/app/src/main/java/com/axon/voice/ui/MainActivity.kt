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
import com.axon.voice.audio.StreamingTts
import com.axon.voice.audio.TtsPlayer
import com.axon.voice.audio.WavRecorder
import com.axon.voice.wake.WakeWordService
import com.google.android.material.materialswitch.MaterialSwitch
import org.json.JSONObject
import java.io.File
import kotlin.concurrent.thread

class MainActivity : AppCompatActivity(), ChatSocket.Listener {

    companion object {
        const val EXTRA_AUTO_LISTEN = "auto_listen"
    }

    private enum class State { IDLE, RECORDING, THINKING, SPEAKING }

    private lateinit var prefs: Prefs
    private lateinit var client: AxonClient
    private var chat: ChatSocket? = null
    private var player: TtsPlayer? = null

    private lateinit var orb: OrbView
    private lateinit var statusLine: TextView
    private lateinit var connLabel: TextView
    private lateinit var wakeSwitch: MaterialSwitch
    private lateinit var transcript: RecyclerView
    private val adapter = TranscriptAdapter()
    private val main = Handler(Looper.getMainLooper())

    private var state = State.IDLE
    private var recorder: WavRecorder? = null
    private var watcher: SilenceWatcher? = null
    private var pendingAutoListen = false
    private var runErrored = false
    /** Active streamed reply, or null when not THINKING/SPEAKING a reply. */
    private var ttsStream: StreamingTts? = null

    private val permLauncher =
        registerForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { grants ->
            if (grants[Manifest.permission.RECORD_AUDIO] == true && pendingAutoListen) {
                pendingAutoListen = false
                startCapture()
            }
        }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        prefs = Prefs(this)
        client = AxonClient(prefs)
        player = TtsPlayer(this)

        orb = findViewById(R.id.orb)
        statusLine = findViewById(R.id.statusLine)
        connLabel = findViewById(R.id.connLabel)
        wakeSwitch = findViewById(R.id.wakeSwitch)
        transcript = findViewById(R.id.transcript)
        transcript.layoutManager = LinearLayoutManager(this).apply { stackFromEnd = true }
        transcript.adapter = adapter

        findViewById<ImageButton>(R.id.settingsBtn).setOnClickListener {
            startActivity(Intent(this, SettingsActivity::class.java))
        }
        findViewById<ImageButton>(R.id.chatBtn).setOnClickListener {
            startActivity(Intent(this, ChatActivity::class.java))
        }
        findViewById<ImageButton>(R.id.newChatBtn).setOnClickListener { newConversation() }
        orb.setOnClickListener { onOrbTap() }

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

        requestPerms()
        handleIntent(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleIntent(intent)
    }

    private fun handleIntent(i: Intent?) {
        val wantsListen = i != null &&
            (i.getBooleanExtra(EXTRA_AUTO_LISTEN, false) || i.action == Intent.ACTION_ASSIST)
        if (!wantsListen) return
        if (!hasMicPermission()) {
            pendingAutoListen = true
            requestPerms()
        } else {
            main.post { if (state == State.IDLE) startCapture() }
        }
    }

    override fun onStart() {
        super.onStart()
        if (!prefs.configured) {
            startActivity(Intent(this, SettingsActivity::class.java))
            return
        }
        // (Re)open the socket — cheap if already connected, and picks up
        // settings edits after a trip to SettingsActivity.
        if (chat == null) {
            chat = ChatSocket(prefs, client.http, this).also { it.open() }
        } else if (chat?.connected != true) {
            chat?.close()
            chat = ChatSocket(prefs, client.http, this).also { it.open() }
        }
    }

    override fun onResume() {
        super.onResume()
        wakeSwitch.isChecked = WakeWordService.running
    }

    override fun onDestroy() {
        chat?.close()
        ttsStream?.abort()
        ttsStream = null
        player?.release()
        // A capture may still be running if we're killed mid-recording; its
        // thread would otherwise keep the mic open with micHold stuck on.
        recorder?.let { runCatching { it.stop() } }
        recorder = null
        WakeWordService.micHold = false
        super.onDestroy()
    }

    /** Start fresh voice + wake conversation threads: new session ids (new
     *  context server-side, new dashboard threads) and a clean transcript. */
    private fun newConversation() {
        when (state) {
            State.RECORDING -> {
                // Discard the in-flight capture; going straight to IDLE would
                // orphan the recorder thread with micHold stuck on.
                val r = recorder
                recorder = null
                watcher = null
                thread(name = "axon-ptt-discard") {
                    runCatching { r?.stop() }
                    WakeWordService.micHold = false
                }
            }

            State.THINKING, State.SPEAKING -> {
                chat?.cancel(prefs.voiceSessionId)
                ttsStream?.abort()
                ttsStream = null
                player?.stop()
            }

            State.IDLE -> {}
        }
        toIdle()
        prefs.newSession("voice")
        prefs.newSession("wake")
        adapter.clear()
        Toast.makeText(this, getString(R.string.new_conversation_started), Toast.LENGTH_SHORT)
            .show()
    }

    // ── Push-to-talk state machine ──────────────────────────────────────────

    private fun onOrbTap() {
        when (state) {
            State.IDLE -> startCapture()
            State.RECORDING -> finishCapture()
            State.THINKING -> {
                chat?.cancel(prefs.voiceSessionId)
                ttsStream?.abort()
                ttsStream = null
                toIdle()
            }

            State.SPEAKING -> {
                ttsStream?.abort()
                ttsStream = null
                player?.stop()
                toIdle()
            }
        }
    }

    private fun startCapture() {
        if (!prefs.configured) {
            startActivity(Intent(this, SettingsActivity::class.java))
            return
        }
        if (!hasMicPermission()) {
            pendingAutoListen = true
            requestPerms()
            return
        }
        if (state != State.IDLE) return
        state = State.RECORDING
        orb.orbState = OrbView.OrbState.LISTENING
        statusLine.text = getString(R.string.status_recording)
        runErrored = false

        val w = SilenceWatcher()
        watcher = w
        val r = WavRecorder()
        recorder = r
        val serviceWasListening = WakeWordService.running
        WakeWordService.micHold = true
        thread(name = "axon-ptt-start") {
            // Give the wake service a beat to release the shared microphone.
            if (serviceWasListening) Thread.sleep(300)
            try {
                r.start { rms ->
                    if (w.tick(rms)) main.post { finishCapture() }
                }
            } catch (e: Exception) {
                // Give the mic back to the wake service — leaving micHold set
                // here would silence "Hey Axon" until the next successful PTT.
                WakeWordService.micHold = false
                main.post {
                    adapter.add("error", e.message ?: "microphone unavailable")
                    scrollEnd()
                    toIdle()
                }
            }
        }
    }

    private fun finishCapture() {
        if (state != State.RECORDING) return
        state = State.THINKING
        orb.orbState = OrbView.OrbState.THINKING
        statusLine.text = getString(R.string.status_thinking)

        val r = recorder
        val w = watcher
        recorder = null
        watcher = null
        thread(name = "axon-ptt-send") {
            val wav = r?.stop()
            WakeWordService.micHold = false
            if (wav == null || w?.hadSpeech != true) {
                main.post { toIdle() }
                return@thread
            }
            val text = try {
                client.transcribe(wav)
            } catch (e: Exception) {
                main.post {
                    adapter.add("error", e.message ?: "transcription failed")
                    scrollEnd()
                    toIdle()
                }
                return@thread
            }
            if (text.isBlank()) {
                main.post { toIdle() }
                return@thread
            }
            main.post {
                adapter.add("user", text)
                adapter.add("assistant", "")
                scrollEnd()
                if (chat?.sendTask(text, prefs.voiceSessionId) != true) {
                    adapter.setLast(getString(R.string.status_offline))
                    toIdle()
                }
            }
        }
    }

    /** Begin a streamed reply: tokens arriving via onWsEvent are fed to TTS
     *  sentence-by-sentence, so the first sentence plays as soon as the agent
     *  has produced it — not after the whole reply is synthesized. */
    private fun startStream() {
        if (state != State.THINKING && state != State.SPEAKING) return
        val p = player ?: return
        ttsStream?.abort()
        ttsStream = StreamingTts(
            player = p,
            client = client,
            cacheDir = cacheDir,
            filePrefix = "reply_ui",
        ) { main.post { if (state == State.SPEAKING) toIdle() } }
    }

    private fun toIdle() {
        state = State.IDLE
        orb.orbState = OrbView.OrbState.IDLE
        statusLine.text = getString(R.string.status_idle)
    }

    // ── Chat socket events ──────────────────────────────────────────────────

    override fun onWsConnected() {
        main.post { connLabel.text = "online" }
    }

    override fun onWsDisconnected() {
        main.post { connLabel.text = "offline" }
    }

    override fun onWsEvent(ev: JSONObject) {
        main.post {
            when (ev.optString("type")) {
                "token" -> if (state == State.THINKING && adapter.lastRole == "assistant") {
                    // Lazy: the first token flips us to SPEAKING and starts
                    // streaming TTS so speech begins immediately, not after done.
                    if (ttsStream == null) {
                        state = State.SPEAKING
                        orb.orbState = OrbView.OrbState.SPEAKING
                        statusLine.text = getString(R.string.status_speaking)
                        startStream()
                    }
                    val text = ev.optString("text")
                    adapter.appendToLast(text)
                    scrollEnd()
                    ttsStream?.append(text)
                }

                "error" -> if (state == State.THINKING || state == State.SPEAKING) {
                    runErrored = true
                    ttsStream?.abort()
                    ttsStream = null
                    adapter.add("error", ev.optString("message", "something went wrong"))
                    scrollEnd()
                    toIdle()
                }

                "done" -> if (state == State.THINKING || state == State.SPEAKING) {
                    if (runErrored) {
                        ttsStream?.abort()
                        ttsStream = null
                        toIdle()
                        return@post
                    }
                    val full = ev.optString("full_text", "")
                    if (adapter.lastRole == "assistant" && adapter.lastText.isBlank() && full.isNotBlank()) {
                        adapter.setLast(full)
                    }
                    scrollEnd()
                    val stream = ttsStream
                    ttsStream = null
                    if (stream != null) {
                        // Tokens were streamed — just flush whatever is buffered.
                        stream.finish()
                    } else if (full.isNotBlank() || adapter.lastText.isNotBlank()) {
                        // Server didn't emit token frames; synthesize the whole
                        // reply in one shot so we're never silent.
                        val text = if (full.isNotBlank()) full else adapter.lastText
                        speakOneShot(text)
                    } else {
                        toIdle()
                    }
                }
            }
        }
    }

    /** Legacy one-shot path for servers that send full_text on done without
     *  any preceding token frames. Kept so a non-streaming backend still
     *  speaks the reply. */
    private fun speakOneShot(text: String) {
        state = State.SPEAKING
        orb.orbState = OrbView.OrbState.SPEAKING
        statusLine.text = getString(R.string.status_speaking)
        thread(name = "axon-speak") {
            val f = File(cacheDir, "reply_ui.audio")
            val ok = client.speech(text, f)
            main.post {
                if (state != State.SPEAKING) return@post
                val p = player ?: return@post
                if (ok) {
                    p.play(f) { main.post { if (state == State.SPEAKING) toIdle() } }
                } else {
                    p.speakFallback(text) { main.post { if (state == State.SPEAKING) toIdle() } }
                }
            }
        }
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

    private fun scrollEnd() {
        if (adapter.itemCount > 0) transcript.scrollToPosition(adapter.itemCount - 1)
    }
}
