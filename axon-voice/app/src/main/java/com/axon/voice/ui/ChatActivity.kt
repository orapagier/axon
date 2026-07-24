package com.axon.voice.ui

import android.Manifest
import android.annotation.SuppressLint
import android.content.Intent
import android.content.pm.PackageManager
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.provider.Settings
import android.util.Log
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
import com.axon.voice.Prefs
import com.axon.voice.R
import com.axon.voice.api.AxonClient
import com.axon.voice.api.ChatSocket
import com.axon.voice.audio.BargeDetector
import com.axon.voice.audio.BargeMonitor
import com.axon.voice.audio.SilenceWatcher
import com.axon.voice.audio.SpeakerEmbedder
import com.axon.voice.audio.StreamingTts
import com.axon.voice.audio.TtsPlayer
import com.axon.voice.audio.VoicePrint
import com.axon.voice.audio.WavRecorder
import com.axon.voice.wake.WakeWordService
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
    }

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
    private lateinit var voiceOverlay: View
    private lateinit var voiceOrb: VoiceOrbView
    private lateinit var voiceOverlayStatus: TextView
    private val adapter = TranscriptAdapter()
    private val main = Handler(Looper.getMainLooper())

    /** Live phase/level from the wake service ([VoiceOverlay]), mirrored onto
     *  the orb while this page is in the foreground. Invoked from service
     *  threads, so every touch of a view is marshalled to the main thread. */
    private val voiceListener = VoiceOverlay.Listener { phase, level ->
        main.post { applyVoiceState(phase, level) }
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

    /** Barge-in for push-to-talk replies: mirrors the hands-free engine in
     *  [WakeWordService], minus its rustpotter listener (this activity has no
     *  wake word of its own to also feed — its own reply is the only thing
     *  playing). One long-lived detector for the activity's lifetime so its
     *  learned echo gain only gets more accurate over time. */
    private val bargeDetector = BargeDetector()

    /** Loaded once, lazily, only if a voiceprint is enrolled — see the same
     *  field in [com.axon.voice.wake.WakeWordService] for why. Null
     *  [voiceprint] means barge-in falls back to energy-only, same as before
     *  this existed. */
    private var speakerEmbedder: SpeakerEmbedder? = null
    private var voiceprint: FloatArray? = null

    /** Bumped every time a voice reply's "speaking" ends, whichever way —
     *  played out naturally, or cut off by [stopSpeaking]. The barge monitor
     *  watching a reply captures its own generation at start time and stops
     *  (releasing the mic back to the wake service) the moment this no longer
     *  matches. Touched from both the main thread and the barge monitor's own
     *  background thread, hence [Volatile]. */
    @Volatile
    private var speakGen = 0

    /** Interruption note for the very next voice send after a barge-in — set
     *  by [onBargeConfirmed], consumed and cleared by [resetInputRow]/
     *  [stopDictation]. Empty when nothing is pending (the common case): it
     *  rides onto the agent task only, never the displayed/saved bubble. */
    private var pendingBargeNote = ""

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

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_chat)

        prefs = Prefs(this)
        client = AxonClient(prefs)
        player = TtsPlayer(this)
        voiceprint = VoicePrint.load(this)
        if (voiceprint != null) {
            speakerEmbedder = runCatching { SpeakerEmbedder(this) }.getOrNull()
        }

        connLabel = findViewById(R.id.connLabel)
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
        // Picks up an enrollment (or a clear) done in Settings since this
        // activity was created — cheap when nothing changed (VoicePrint.load
        // is a fast file-exists check), and a fresh SpeakerEmbedder is only
        // built when there's a new voiceprint to actually use.
        if (voiceprint == null) {
            voiceprint = VoicePrint.load(this)
            if (voiceprint != null) {
                speakerEmbedder = runCatching { SpeakerEmbedder(this) }.getOrNull()
            }
        }
    }

    override fun onStop() {
        if (VoiceOverlay.listener === voiceListener) VoiceOverlay.listener = null
        voiceOrb.setPhase(VoiceOrbView.Phase.IDLE) // stop the animation loop
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
        speakerEmbedder?.close()
        speakerEmbedder = null
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
            voiceOverlay.visibility = View.GONE
            voiceOrb.setPhase(VoiceOrbView.Phase.IDLE)
            return
        }
        voiceOverlay.visibility = View.VISIBLE
        voiceOrb.setPhase(
            when (phase) {
                VoiceOverlay.Phase.LISTENING -> VoiceOrbView.Phase.LISTENING
                VoiceOverlay.Phase.THINKING -> VoiceOrbView.Phase.THINKING
                else -> VoiceOrbView.Phase.SPEAKING
            }
        )
        voiceOverlayStatus.setText(
            when (phase) {
                VoiceOverlay.Phase.LISTENING -> R.string.status_recording
                VoiceOverlay.Phase.THINKING -> R.string.status_thinking
                else -> R.string.status_speaking
            }
        )
        if (level >= 0f) voiceOrb.setLevel(level)
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
                // Captured before resetInputRow (which clears it) so a
                // barge-in's interruption note rides onto this send only.
                val bargePrefix = pendingBargeNote
                resetInputRow()
                if (text.isNotBlank()) sendMessage(text, voice = true, taskPrefix = bargePrefix)
            }
        }
    }

    /** Back to the composable state after dictation ends, however it ended.
     *  Also drops any pending barge-in interruption note: whether this
     *  capture succeeds or is abandoned (no speech, an error), the note must
     *  not survive to ride along on some unrelated future send. */
    private fun resetInputRow() {
        if (state == State.RECORDING || state == State.TRANSCRIBING) state = State.IDLE
        micBtn.clearColorFilter()
        micBtn.alpha = 1f
        input.hint = getString(R.string.chat_hint)
        pendingBargeNote = ""
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
     *  [voice] marks a push-to-talk send, whose reply is also read aloud.
     *  [taskPrefix] rides onto the task sent to the agent only — e.g. a
     *  barge-in's interruption note — never into the displayed/saved bubble. */
    private fun sendMessage(text: String, voice: Boolean = false, taskPrefix: String = "") {
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
            val gen = ++speakGen
            replyTts = StreamingTts(p, client, cacheDir, "reply_chat") {
                main.post { speakGen++ }
            }
            // Watch for a talk-over interruption only if the user has barge-in
            // on; off, the reply just plays out and they wait for it to finish.
            if (prefs.bargeInEnabled) startBargeMonitor(gen)
        }
        if (chat?.sendTask(taskPrefix + text, prefs.chatSessionId, voice) != true) {
            adapter.setAt(streamIdx, getString(R.string.status_offline))
            streamIdx = -1
            state = State.IDLE
            stopSpeaking()
        }
    }

    /** Silence a reply being read aloud — a new send, stop, new conversation,
     *  or the user reaching for the mic all take the speaker back. */
    private fun stopSpeaking() {
        speakGen++ // invalidate any barge monitor still watching this reply
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
            // s's own onDone (wired in sendMessage) closes out speakGen once
            // this drains — the barge monitor keeps watching until then.
            s.finish()
            return
        }
        s.abort() // does not fire onDone — this path speaks separately, below
        val p = player
        if (full.isBlank() || p == null) {
            speakGen++ // nothing to play — this reply's "speaking" is already over
            return
        }
        thread(name = "axon-chat-tts") {
            val f = File(cacheDir, "reply_chat_full.audio")
            val ok = runCatching { client.speech(full, f) }.getOrDefault(false)
            main.post {
                if (ok && f.length() > 0) {
                    p.play(f) { main.post { speakGen++ } }
                } else {
                    p.speakFallback(full) { main.post { speakGen++ } }
                }
            }
        }
    }

    // ── Barge-in (interrupt a voice reply while it's speaking) ──────────────

    /** Starts watching the mic for a barge-in while a voice reply plays.
     *  Borrows the wake service's mic ([WakeWordService.micHold]) since only
     *  one [AudioRecord] can capture reliably at a time — mirrors how
     *  [startDictation] already does this for push-to-talk. [gen] is this
     *  reply's [speakGen] snapshot: the monitor stops the instant it no
     *  longer matches, whether that's a natural end or an abort. */
    private fun startBargeMonitor(gen: Int) {
        bargeDetector.reset()
        WakeWordService.micHold = true
        thread(name = "axon-chat-barge") {
            val rec = openBargeRecord()
            if (rec == null) {
                WakeWordService.micHold = false
                return@thread
            }
            // Only released here on a natural end / plain stop. A confirmed
            // barge-in hands straight off to dictation, which needs the mic
            // held continuously — releasing it in the gap before
            // onBargeConfirmed runs on the main thread would let the wake
            // service race back in and reclaim it. stopDictation() (or its
            // own error path) is what finally releases it in that case.
            var confirmed = false
            BargeMonitor(
                detector = bargeDetector,
                wakeDetector = null, // no wake-word listener shares this mic — our own reply is playing
                readFrame = { f -> fillBargeFrame(rec, f, gen) },
                onTentative = { player?.duck(); Log.d("BargeDetector", "barge tentative (ducked): ${bargeDetector.diagnostics()}") },
                onFalseAlarm = { player?.restoreVolume(); Log.d("BargeDetector", "barge false-alarm (restored): ${bargeDetector.diagnostics()}") },
                onConfirmed = { preroll ->
                    Log.d("BargeDetector", "barge CONFIRMED: ${bargeDetector.diagnostics()}")
                    confirmed = true
                    main.post { onBargeConfirmed(gen, preroll) }
                },
            ).run { gen != speakGen }
            rec.release()
            if (!confirmed) WakeWordService.micHold = false
        }
    }

    @SuppressLint("MissingPermission")
    private fun openBargeRecord(): AudioRecord? {
        val minBuf = AudioRecord.getMinBufferSize(
            WavRecorder.SAMPLE_RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT
        )
        val rec = try {
            AudioRecord(
                MediaRecorder.AudioSource.VOICE_RECOGNITION,
                WavRecorder.SAMPLE_RATE,
                AudioFormat.CHANNEL_IN_MONO,
                AudioFormat.ENCODING_PCM_16BIT,
                maxOf(minBuf * 2, WavRecorder.SAMPLE_RATE * 2)
            )
        } catch (_: Exception) {
            return null
        }
        if (rec.state != AudioRecord.STATE_INITIALIZED) {
            rec.release()
            return null
        }
        rec.startRecording()
        return rec
    }

    /** Blocking-fill one frame; false once [gen] is stale (this session ended,
     *  one way or another) or the read dies. */
    private fun fillBargeFrame(rec: AudioRecord, frame: ShortArray, gen: Int): Boolean {
        var off = 0
        while (off < frame.size && gen == speakGen) {
            val n = rec.read(frame, off, frame.size - off)
            if (n <= 0) return false
            off += n
        }
        return off == frame.size
    }

    /** A confirmed barge-in: cut the reply off, cancel the run if it's still
     *  generating, and roll straight into a capture seeded with [preroll] —
     *  no ack, no settle, the user is already mid-sentence. Runs on the main
     *  thread (posted by [startBargeMonitor]). */
    private fun onBargeConfirmed(gen: Int, preroll: ByteArray) {
        if (gen != speakGen) {
            // Stale — something else (a new send, a manual stop) already ran
            // on the main thread before this posted callback got its turn.
            // The barge thread deliberately left the mic held for us to hand
            // off to dictation; since we're not taking it, release it here
            // instead of leaking it held forever.
            WakeWordService.micHold = false
            return
        }
        // Read before stopSpeaking() aborts the stream — abort doesn't erase
        // what already finished playing, but grab it first for clarity.
        val spoken = replyTts?.spokenSoFar().orEmpty()
        stopSpeaking()
        if (state == State.WAITING) {
            // Mirrors onSendTap's manual stop: cancel now rather than wait for
            // a server event, so the composer unblocks immediately.
            chat?.cancel(prefs.chatSessionId)
            state = State.IDLE
            streamIdx = -1
            ChatHistory.save(this, prefs.chatSessionId, adapter.snapshot())
        }
        pendingBargeNote = if (spoken.isNotBlank()) {
            "(Note: I interrupted your previous reply mid-speech; I heard only up to: \"${clip(spoken)}\") "
        } else {
            "(Note: I interrupted your previous reply before you said anything.) "
        }
        startBargedDictation(preroll)
    }

    /** One-line, length-capped copy of the spoken-so-far text for the
     *  interruption note. */
    private fun clip(s: String, max: Int = 200): String {
        val t = s.trim().replace(Regex("\\s+"), " ")
        return if (t.length <= max) t else t.take(max).trimEnd() + "…"
    }

    /** Like [startDictation], but for a confirmed barge-in: the mic is
     *  already held (this activity's own barge monitor had it, and
     *  [WakeWordService.micHold] stays true through this handoff), so there's
     *  no wake service to wait out and no permission/configuration checks to
     *  repeat mid-conversation. Captures immediately, seeded with what the
     *  user said in the time it took the barge-in to confirm. */
    private fun startBargedDictation(preroll: ByteArray) {
        state = State.RECORDING
        micBtn.setColorFilter(ContextCompat.getColor(this, R.color.error))
        input.hint = getString(R.string.chat_hint_listening)

        val w = SilenceWatcher()
        watcher = w
        val r = WavRecorder()
        recorder = r
        thread(name = "axon-dictate-start") {
            try {
                r.start(preroll = preroll) { rms ->
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
