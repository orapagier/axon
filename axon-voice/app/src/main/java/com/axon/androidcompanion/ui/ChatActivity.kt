package com.axon.androidcompanion.ui

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
import android.view.View
import android.widget.EditText
import android.widget.ImageButton
import android.widget.TextView
import android.widget.Toast
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.ContextCompat
import androidx.recyclerview.widget.LinearLayoutManager
import androidx.recyclerview.widget.RecyclerView
import com.axon.androidcompanion.Prefs
import com.axon.androidcompanion.R
import com.axon.androidcompanion.api.AxonClient
import com.axon.androidcompanion.api.ChatSocket
import com.axon.androidcompanion.audio.SilenceWatcher
import com.axon.androidcompanion.audio.StreamingTts
import com.axon.androidcompanion.audio.TtsPlayer
import com.axon.androidcompanion.audio.WavRecorder
import com.axon.androidcompanion.device.AppConfig
import com.axon.androidcompanion.device.ApiService
import com.axon.androidcompanion.wake.WakeWordService
import com.google.android.material.switchmaterial.SwitchMaterial
import org.json.JSONObject
import java.io.File
import kotlin.concurrent.thread

/**
 * The app's home screen: type a message, or tap the mic to speak one. The
 * recording (silence-watched push-to-talk) is transcribed server-side and
 * sent as its own chat message, never left in the composer.
 *
 * Speaking to Axon gets a spoken answer: a push-to-talk reply is read aloud
 * through [StreamingTts] as it streams, exactly as the "Hey Axon" wake service
 * reads its own. A typed message is answered in text alone — the same rule the
 * dashboard applies, so the keyboard never makes the phone talk.
 *
 * Runs on [Prefs.chatSessionId] for typed and push-to-talk messages. Each
 * "Hey Axon" wake is its own separate thread ([Prefs.newWakeConversationId])
 * rather than joining this one — [ChatFeed] only mirrors an exchange into this
 * page's live list when it happens to match [Prefs.chatSessionId], which a
 * wake conversation never does. [HistoryActivity] (the toolbar's history icon)
 * is where those hands-free conversations are actually reviewed. The wake
 * button in the input row toggles the hands-free listener without leaving the
 * page.
 *
 * Launching with [EXTRA_AUTO_LISTEN] (or via the system assist gesture) starts
 * dictation immediately — the power-button assistant flow lands here.
 */
class ChatActivity : AppCompatActivity(), ChatSocket.Listener {

    companion object {
        const val EXTRA_AUTO_LISTEN = "auto_listen"

        /** Grace period before an IDLE phase actually hides the orb. A brief
         *  IDLE between phases (a socket reconnect blip, a phase race) shouldn't
         *  blink the orb out mid-conversation — any real phase arriving within
         *  this window cancels the pending hide. */
        private const val ORB_HIDE_DELAY_MS = 500L
    }

    private enum class State { IDLE, RECORDING, TRANSCRIBING, WAITING }

    private lateinit var prefs: Prefs
    private lateinit var client: AxonClient
    private var chat: ChatSocket? = null

    private lateinit var connLabel: TextView
    private lateinit var deviceControlToggle: SwitchMaterial
    private lateinit var wakeBtn: ImageButton
    private lateinit var input: EditText
    private lateinit var micBtn: ImageButton
    private lateinit var sendBtn: ImageButton
    private lateinit var list: RecyclerView
    private lateinit var voiceOverlay: View
    private lateinit var voiceOrb: VoiceOrbView
    private lateinit var voiceOverlayStatus: TextView
    private val adapter = TranscriptAdapter()
    private val main = Handler(Looper.getMainLooper())

    /** Live phase/level from the wake service ([VoiceOverlay]), mirrored onto
     *  the orb while this page is in the foreground. Invoked from service
     *  threads, so every touch of a view is marshalled to the main thread. */
    private val voiceListener = object : VoiceOverlay.Listener {
        override fun onState(phase: VoiceOverlay.Phase, level: Float) {
            main.post { applyVoiceState(phase, level) }
        }

        override fun onPaused(paused: Boolean) {
            main.post { applyPaused(paused) }
        }
    }

    /** The orb phase currently shown, so [applyVoiceState] only touches the orb
     *  and status text when the phase actually changes — it's called ~50×/sec
     *  during speech (one per reply-audio level sample) and rewriting the view
     *  every time is needless main-thread churn. Null = orb hidden. */
    private var shownPhase: VoiceOverlay.Phase? = null

    /** Deferred hide for the debounce in [applyVoiceState]; see [ORB_HIDE_DELAY_MS]. */
    private val hideOrb = Runnable {
        voiceOverlay.visibility = View.GONE
        voiceOrb.setPhase(VoiceOrbView.Phase.IDLE)
        voiceOrb.setPaused(false)
        shownPhase = null
    }

    private var state = State.IDLE
    private var recorder: WavRecorder? = null
    private var watcher: SilenceWatcher? = null
    private var pendingDictate = false
    private var pendingWake = false

    private var player: TtsPlayer? = null

    /** Non-null while a voice-initiated run streams its reply into TTS. Set by
     *  push-to-talk sends only: like the dashboard, speaking to Axon gets a
     *  spoken answer, while a typed message is answered in text alone. */
    private var replyTts: StreamingTts? = null

    /** Adapter index of the assistant bubble the current run streams into.
     *  Index-addressed (not "last item") because a wake-word exchange can be
     *  appended below it mid-stream via [ChatFeed]. */
    private var streamIdx = -1

    /** Live inserts from the wake service — its exchange is already persisted
     *  by [ChatFeed.post]; this only mirrors it into the open list. */
    private val feedListener = ChatFeed.Listener { sessionId, role, text ->
        // Only mirror exchanges for the conversation this page is showing.
        // Hands-free ("Hey Axon") turns live in their own per-wake
        // conversations, so they must not be interleaved into — or persisted
        // under — the manual chat thread via this page's snapshot saves.
        if (sessionId != prefs.chatSessionId) return@Listener
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

    private val enrollLauncher =
        registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
            if (result.resultCode == RESULT_OK) setWakeEnabled(true) else updateWakeBtn()
        }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_chat)

        prefs = Prefs(this)
        client = AxonClient(prefs)
        // Push-to-talk replies play out to completion; the user interrupts one
        // with Stop, or by reaching for the mic (which stops playback first).
        player = TtsPlayer(this)

        connLabel = findViewById(R.id.connLabel)
        deviceControlToggle = findViewById(R.id.deviceControlToggle)
        wakeBtn = findViewById(R.id.wakeBtn)
        input = findViewById(R.id.chatInput)
        micBtn = findViewById(R.id.micBtn)
        sendBtn = findViewById(R.id.sendBtn)
        list = findViewById(R.id.chatList)
        list.layoutManager = LinearLayoutManager(this).apply { stackFromEnd = true }
        list.adapter = adapter

        voiceOverlay = findViewById(R.id.voiceOverlay)
        voiceOrb = findViewById(R.id.voiceOrb)
        voiceOverlayStatus = findViewById(R.id.voiceOverlayStatus)

        adapter.load(ChatHistory.load(this, prefs.chatSessionId))
        scrollEnd()
        ChatFeed.listener = feedListener

        findViewById<ImageButton>(R.id.settingsBtn).setOnClickListener {
            startActivity(Intent(this, SettingsActivity::class.java))
        }
        findViewById<ImageButton>(R.id.historyBtn).setOnClickListener {
            startActivity(Intent(this, HistoryActivity::class.java))
        }
        findViewById<ImageButton>(R.id.newChatBtn).setOnClickListener { newConversation() }
        micBtn.setOnClickListener { onMicTap() }
        sendBtn.setOnClickListener { onSendTap() }
        wakeBtn.setOnClickListener { setWakeEnabled(!WakeWordService.running) }
        deviceControlToggle.setOnCheckedChangeListener { _, checked -> setDeviceControlEnabled(checked) }

        // Orb gestures: tap the core to hold/resume the reply + follow-up window,
        // tap anywhere outside it (the orb periphery or the scrim) to close the
        // exchange. The service does the work via VoiceOverlay.controller.
        voiceOrb.onCenterTap = { VoiceOverlay.controller?.onTogglePause() }
        voiceOrb.onOutsideTap = { VoiceOverlay.controller?.onClose() }
        voiceOverlay.setOnClickListener { VoiceOverlay.controller?.onClose() }

        handleIntent(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleIntent(intent)
    }

    /** Assist gesture / EXTRA_AUTO_LISTEN → jump straight into dictation. */
    private fun handleIntent(i: Intent?) {
        val wantsListen = i != null &&
            (i.getBooleanExtra(EXTRA_AUTO_LISTEN, false) || i.action == Intent.ACTION_ASSIST)
        if (!wantsListen) return
        if (!hasMicPermission()) {
            pendingDictate = true
            requestPerms()
        } else {
            main.post { if (state == State.IDLE) startDictation() }
        }
    }

    override fun onStart() {
        super.onStart()
        // Observe the wake service's hands-free phase only while visible — the
        // orb is a foreground affordance, and animating it off-screen would
        // just burn battery. Sync to the current phase in case a wake landed
        // (or is mid-flight) while this page was away.
        VoiceOverlay.listener = voiceListener
        applyVoiceState(VoiceOverlay.phase, -1f)
        applyPaused(VoiceOverlay.paused)
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
        deviceControlToggle.isChecked = ApiService.isRunning
    }

    override fun onStop() {
        if (VoiceOverlay.listener === voiceListener) VoiceOverlay.listener = null
        main.removeCallbacks(hideOrb)
        voiceOrb.setPhase(VoiceOrbView.Phase.IDLE) // stop the animation loop
        // Force onStart's applyVoiceState to re-assert the phase (and restart the
        // orb) even if the service is still on the same phase we left on.
        shownPhase = null
        ChatHistory.save(this, prefs.chatSessionId, adapter.snapshot())
        super.onStop()
    }

    override fun onDestroy() {
        if (VoiceOverlay.listener === voiceListener) VoiceOverlay.listener = null
        if (ChatFeed.listener === feedListener) ChatFeed.listener = null
        if (state == State.RECORDING) {
            recorder?.let { runCatching { it.stop() } }
            recorder = null
            WakeWordService.micHold = false
        }
        stopSpeaking()
        player?.release()
        player = null
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
            // First time on with nothing enrolled: build a personal model
            // before ever starting the service — resumes here via
            // enrollLauncher on success.
            if (!prefs.wakeEnrolled) {
                enrollLauncher.launch(Intent(this, EnrollWakeWordActivity::class.java))
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

    // ── Device control (local HTTP API + Cloudflare tunnel), one switch ────────
    // CloudflaredManager is started/stopped from within ApiService's own
    // startup()/shutdown(), so toggling just the service already covers both.

    private fun setDeviceControlEnabled(on: Boolean) {
        AppConfig.setDeviceControlEnabled(this, on)
        val intent = Intent(this, ApiService::class.java)
        if (on) {
            intent.action = ApiService.ACTION_START
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) startForegroundService(intent)
            else startService(intent)
        } else {
            intent.action = ApiService.ACTION_STOP
            startService(intent)
        }
    }

    // ── Hands-free orb overlay ──────────────────────────────────────────────

    /** Reflect the wake service's current phase onto the orb. IDLE hides the
     *  overlay and ends one exchange; every other phase shows it and feeds the
     *  reactive listening level. The orb stays up for the whole exchange — it is
     *  no longer tap-dismissible, since a full-screen tap target meant a single
     *  stray touch during a long hands-free conversation hid it for the rest of
     *  that conversation (it only came back on IDLE), which read as "the orb
     *  just disappeared." */
    private fun applyVoiceState(phase: VoiceOverlay.Phase, level: Float) {
        if (phase == VoiceOverlay.Phase.IDLE) {
            // Debounce the hide so a momentary IDLE doesn't blink the orb out —
            // a genuine end still hides it ORB_HIDE_DELAY_MS later.
            main.removeCallbacks(hideOrb)
            main.postDelayed(hideOrb, ORB_HIDE_DELAY_MS)
            return
        }
        main.removeCallbacks(hideOrb)
        voiceOverlay.visibility = View.VISIBLE
        if (phase != shownPhase) {
            shownPhase = phase
            voiceOrb.setPhase(
                when (phase) {
                    VoiceOverlay.Phase.LISTENING -> VoiceOrbView.Phase.LISTENING
                    VoiceOverlay.Phase.THINKING -> VoiceOrbView.Phase.THINKING
                    else -> VoiceOrbView.Phase.SPEAKING
                }
            )
            voiceOverlayStatus.setText(
                if (VoiceOverlay.paused) R.string.status_paused else statusRes(phase)
            )
        }
        if (level >= 0f) voiceOrb.setLevel(level)
    }

    private fun statusRes(phase: VoiceOverlay.Phase): Int = when (phase) {
        VoiceOverlay.Phase.LISTENING -> R.string.status_recording
        VoiceOverlay.Phase.THINKING -> R.string.status_thinking
        else -> R.string.status_speaking
    }

    /** Reflect the exchange's hold state on the orb: the pause glyph + resting
     *  glow, and a "Paused" label that reverts to the live phase on resume. */
    private fun applyPaused(paused: Boolean) {
        voiceOrb.setPaused(paused)
        val phase = shownPhase
        voiceOverlayStatus.setText(
            when {
                paused -> R.string.status_paused
                phase != null -> statusRes(phase)
                else -> R.string.status_recording
            }
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
        // Don't talk over the user, and don't let the reply we are reading
        // aloud bleed into the capture and be transcribed as their command.
        stopSpeaking()
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
                if (text.isNotBlank()) sendMessage(text, voice = true)
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
            stopSpeaking()
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
     *  the user bubble, open a streaming assistant bubble, ship the task.
     *  [voice] marks a push-to-talk send, whose reply is also read aloud. */
    private fun sendMessage(text: String, voice: Boolean = false) {
        if (state != State.IDLE) return
        adapter.add("user", text)
        adapter.add("assistant", "")
        streamIdx = adapter.lastIndex
        scrollEnd()
        state = State.WAITING
        ChatHistory.save(this, prefs.chatSessionId, adapter.snapshot())
        // A previous reply still being read aloud yields to the new request.
        stopSpeaking()
        val p = player
        if (voice && p != null) {
            // Distinct file prefix: the wake service synthesizes into the same
            // cache dir and must not collide with this stream.
            replyTts = StreamingTts(p, client, cacheDir, "reply_chat") {}
        }
        if (chat?.sendTask(text, prefs.chatSessionId, voice) != true) {
            adapter.setAt(streamIdx, getString(R.string.status_offline))
            streamIdx = -1
            state = State.IDLE
            stopSpeaking()
        }
    }

    /** Silence a reply being read aloud — a new send, stop, new conversation,
     *  or the user reaching for the mic all take the speaker back. */
    private fun stopSpeaking() {
        replyTts?.abort()
        replyTts = null
        player?.stop()
    }

    /**
     * Close out the read-aloud stream for a finished run. Mirrors the wake
     * service: the server delivers a reply as one token frame followed
     * immediately by done, so finish() must be what ends playback — and a run
     * that emitted no token frame at all has nothing queued, where finalizing
     * an empty stream would say nothing. Synthesize full_text in one blob then.
     */
    private fun finishSpeaking(full: String) {
        val s = replyTts ?: return
        replyTts = null
        if (s.hasContent) {
            s.finish()
            return
        }
        s.abort() // does not fire onDone — this path speaks separately, below
        val p = player
        if (full.isBlank() || p == null) return
        thread(name = "axon-chat-tts") {
            val f = File(cacheDir, "reply_chat_full.audio")
            val ok = runCatching { client.speech(full, f) }.getOrDefault(false)
            main.post {
                if (ok && f.length() > 0) {
                    p.play(f) {}
                } else {
                    p.speakFallback(full) {}
                }
            }
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
                stopSpeaking()
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
                    val text = ev.optString("text")
                    adapter.appendAt(streamIdx, text)
                    replyTts?.append(text)
                    scrollEnd()
                }

                "done" -> if (state == State.WAITING) {
                    val full = ev.optString("full_text", "")
                    if (streamIdx >= 0 && adapter.textAt(streamIdx).isBlank() && full.isNotBlank()) {
                        adapter.setAt(streamIdx, full)
                    }
                    finishSpeaking(full)
                    scrollEnd()
                    state = State.IDLE
                    streamIdx = -1
                    ChatHistory.save(this, prefs.chatSessionId, adapter.snapshot())
                }

                "error" -> if (state == State.WAITING) {
                    stopSpeaking()
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
        stopSpeaking()
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
