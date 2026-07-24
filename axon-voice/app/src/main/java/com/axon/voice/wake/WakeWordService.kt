package com.axon.voice.wake

import android.annotation.SuppressLint
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.graphics.drawable.Icon
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.util.Log
import com.axon.voice.Prefs
import com.axon.voice.R
import com.axon.voice.api.AxonClient
import com.axon.voice.api.ChatSocket
import com.axon.voice.audio.BargeDetector
import com.axon.voice.audio.BargeMonitor
import com.axon.voice.audio.SPEAKER_SIMILARITY_THRESHOLD
import com.axon.voice.audio.Sound
import com.axon.voice.audio.SilenceWatcher
import com.axon.voice.audio.SpeakerEmbedder
import com.axon.voice.audio.StreamingTts
import com.axon.voice.audio.TtsPlayer
import com.axon.voice.audio.VoicePrint
import com.axon.voice.audio.VoicePrompts
import com.axon.voice.audio.WavRecorder
import com.axon.voice.audio.cosineSimilarity
import com.axon.voice.audio.speakerVerifier
import com.axon.voice.ui.ChatFeed
import com.axon.voice.ui.ChatActivity
import com.axon.voice.ui.VoiceOverlay
import org.json.JSONObject
import java.io.ByteArrayOutputStream
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread
import kotlin.math.sqrt

/**
 * Always-on "Hey Axon" listener: a microphone foreground service running the
 * rustpotter detector on a continuous 16k capture. On detection it speaks the
 * "Yes?" ack, captures the command with the ported silence watcher, ships it
 * through transcribe -> /ws task -> TTS, then opens the follow-up window
 * (soft chime, no spoken prompt) with the raised bystander bar — the same flow
 * as the dashboard wake word, minus the browser.
 *
 * Hands-free runs on the Chat page's session id, and every exchange is posted
 * through [ChatFeed]: it lands in the same saved conversation as typed chat,
 * live in the open Chat page, so spoken requests still leave links and text
 * you can go back to.
 */
class WakeWordService : Service(), ChatSocket.Listener {

    companion object {
        private const val LOG_TAG = "WakeWordService"
        private const val CHANNEL = "axon_voice"
        private const val NOTIF_ID = 1
        const val ACTION_STOP = "com.axon.voice.STOP_WAKE"

        /** MediaPlayer's completion callback can fire a few hundred ms before
         *  the audio sink actually finishes playing out (worse on Bluetooth).
         *  Waiting this long before the pre-capture drain keeps our own ack /
         *  reply tail out of the follow-up capture. */
        private const val AUDIO_SETTLE_MS = 250L

        /** How much raw mic audio [runLoop] keeps buffered purely so a wake
         *  hit has something to run [SpeakerEmbedder] against — rustpotter's
         *  own scoring window (it fires once "Hey Axon" already matched) isn't
         *  exposed to us, so this is captured independently, frame by frame,
         *  rather than reusing whatever internal buffer rustpotter has. ~1.6s
         *  comfortably covers the two-word phrase plus a little lead-in. */
        private const val WAKE_PREROLL_SAMPLES = WavRecorder.SAMPLE_RATE * 8 / 5

        /** Diagnostic-only for now: [passesSpeakerCheck] still runs and logs
         *  its similarity score on every wake hit, but doesn't gate whether
         *  [interact] fires. First real-device run of this gate silenced the
         *  wake word outright — comparing a ~1-1.5s "Hey Axon" embedding
         *  against a voiceprint averaged from full-sentence enrollment takes
         *  is a much noisier match than embedding-to-embedding comparisons of
         *  similar-length clips, and this was never validated on hardware
         *  before shipping (no device/emulator was available). Flip this back
         *  on once logcat shows real similarity numbers for genuine attempts
         *  vs. strangers, so the threshold (or preroll length) can be picked
         *  from data instead of guessed twice. */
        private const val WAKE_SPEAKER_GATE_ENABLED = false

        @Volatile
        var running = false
            private set

        /** Set by the UI to borrow the microphone for push-to-talk; the service
         *  releases its AudioRecord until cleared. */
        @Volatile
        var micHold = false

        fun start(ctx: Context) {
            ctx.startForegroundService(Intent(ctx, WakeWordService::class.java))
        }

        fun stop(ctx: Context) {
            ctx.stopService(Intent(ctx, WakeWordService::class.java))
        }
    }

    private lateinit var prefs: Prefs
    private lateinit var client: AxonClient
    private var chat: ChatSocket? = null
    private var player: TtsPlayer? = null
    private var wakeLock: PowerManager.WakeLock? = null
    private var worker: Thread? = null

    @Volatile
    private var alive = false

    /** Cached server-TTS audio per wake ack. Phrases whose prefetch failed are
     *  absent and retried on the next service start — the resilience mirror of
     *  voiceprompts.js's cache/inflight map. Concurrent: the prefetch thread
     *  writes while the wake thread reads. */
    private val promptFiles = ConcurrentHashMap<String, File>()

    // One reply in flight at a time. The reply is *streamed*: each token is
    // fed to [replyStream] for per-sentence TTS so speech starts with the first
    // sentence, not after the whole reply arrives. replyLatch counts down once
    // playback drains (or on error), and replyText holds the full text for the
    // self-echo check on the next capture.
    private val replyLock = Object()
    private var replyLatch: CountDownLatch? = null
    private var replyText: String? = null
    private var replyError: String? = null
    private var replyStream: StreamingTts? = null

    /** The last up-to-2 completed turns of the *previous* wake conversation,
     *  offered as an optional hint (not loaded history) at the start of the
     *  next one — so a fresh "Hey Axon" can pick up a thread if the user refers
     *  back, but starts clean otherwise. Only ever touched from the wake
     *  thread's interact(), so it needs no lock. */
    private val previousTurns = ArrayList<Pair<String, String>>()

    /** One long-lived barge-in detector for the service's whole lifetime, not
     *  one per reply: its learned echo gain ([BargeDetector.reset] keeps it)
     *  only gets more accurate the longer it listens to this device's own
     *  speaker-into-mic coupling. [awaitStreamBlocking] resets its per-turn
     *  state at the start of every new reply. */
    private val bargeDetector = BargeDetector()

    /** Loaded once, lazily, only if a voiceprint is enrolled — a
     *  [SpeakerEmbedder] loads ~28MB of model weights, not worth paying for
     *  on a device where speaker verification was never set up (Settings >
     *  Voice ID). Gates both the wake word itself (see [passesSpeakerCheck])
     *  and a mid-reply barge-in (see [awaitStreamBlocking]'s [BargeMonitor]).
     *  Null [voiceprint] means [speakerVerifier] returns null too, and both
     *  fall back to keyword/energy-only, same as before this existed. */
    private var speakerEmbedder: SpeakerEmbedder? = null
    private var voiceprint: FloatArray? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        prefs = Prefs(this)
        client = AxonClient(prefs)
        // The reply-audio RMS drives the SPEAKING orb in realtime, and feeds
        // the barge-in detector's echo reference — speakLevel ignores it
        // outside the speaking phase (so ack playback won't leak into the
        // orb), but the detector needs it unconditionally: it's reset per-turn
        // in awaitStreamBlocking rather than gated by phase.
        player = TtsPlayer(this).apply {
            onLevel = { rms ->
                VoiceOverlay.speakLevel(rms)
                bargeDetector.feedPlayback(rms)
            }
        }
        voiceprint = VoicePrint.load(this)
        if (voiceprint != null) {
            speakerEmbedder = runCatching { SpeakerEmbedder(this) }.getOrNull()
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopSelf()
            return START_NOT_STICKY
        }
        createChannel()
        startForeground(NOTIF_ID, notif(getString(R.string.notif_listening)))
        if (!alive) {
            alive = true
            running = true
            wakeLock = (getSystemService(POWER_SERVICE) as PowerManager)
                .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "axon:wake")
                .apply {
                    setReferenceCounted(false)
                    acquire()
                }
            chat = ChatSocket(prefs, client.http, this).also { it.open() }
            worker = thread(name = "axon-wake") { runLoop() }
        }
        return START_STICKY
    }

    override fun onDestroy() {
        alive = false
        running = false
        VoiceOverlay.setPhase(VoiceOverlay.Phase.IDLE)
        chat?.close()
        chat = null
        player?.release()
        player = null
        wakeLock?.release()
        wakeLock = null
        worker?.join(1500)
        worker = null
        speakerEmbedder?.close()
        speakerEmbedder = null
        super.onDestroy()
    }

    // ── Detection loop ──────────────────────────────────────────────────────

    private fun runLoop() {
        if (!WakeDetector.available) {
            fail("Wake engine missing from this build")
            return
        }
        val model = try {
            assets.open("heyaxon.rpw").readBytes()
        } catch (_: Exception) {
            fail("Wake model asset missing")
            return
        }
        val detector = try {
            WakeDetector(model)
        } catch (e: Exception) {
            fail(e.message ?: "Wake model rejected")
            return
        }
        // Off the wake thread: each miss burns a network timeout, and "Hey
        // Axon" must be listening immediately, not after a run of slow fetches.
        thread(name = "axon-prompt-prefetch") { prefetchPrompts() }

        var record: AudioRecord? = null
        // Rolling raw-PCM ring buffer of the last ~WAKE_PREROLL_SAMPLES, so a
        // detection has recent audio to verify against — see the constant's
        // doc and passesSpeakerCheck.
        val wakePreroll = ArrayDeque<ShortArray>()
        var wakePrerollSamples = 0
        try {
            val frame = ShortArray(detector.samplesPerFrame)
            while (alive) {
                if (micHold) {
                    record?.release()
                    record = null
                    Thread.sleep(100)
                    continue
                }
                var rec = record
                if (rec == null) {
                    rec = openRecord()
                    if (rec == null) {
                        Thread.sleep(1000)
                        continue
                    }
                    record = rec
                }
                if (!fillFrame(rec, frame)) continue
                wakePreroll.addLast(frame.copyOf())
                wakePrerollSamples += frame.size
                while (wakePrerollSamples > WAKE_PREROLL_SAMPLES && wakePreroll.size > 1) {
                    wakePrerollSamples -= wakePreroll.removeFirst().size
                }
                val score = detector.process(frame)
                if (score >= 0f) {
                    // See WAKE_SPEAKER_GATE_ENABLED: runs and logs unconditionally
                    // right now, gates interact() only once it's proven out.
                    val speakerOk = passesSpeakerCheck(wakePreroll)
                    if (!WAKE_SPEAKER_GATE_ENABLED || speakerOk) {
                        interact(rec, detector)
                        drain(rec)
                    }
                    wakePreroll.clear()
                    wakePrerollSamples = 0
                }
            }
        } finally {
            record?.release()
            detector.close()
        }
    }

    /** True if nothing is enrolled (same energy/keyword-only fallback
     *  [BargeMonitor.verifySpeaker] uses), or if the enrolled voice matches
     *  [preroll] — the ~1.6s of raw mic audio leading up to and including the
     *  phrase that just triggered rustpotter. Logs the raw cosine similarity
     *  either way (see [WAKE_SPEAKER_GATE_ENABLED]) instead of going through
     *  [speakerVerifier]'s opaque true/false, so real on-device numbers are
     *  visible in logcat. Never lets a broken check silence the wake word
     *  outright: any exception is logged and treated as a pass. */
    private fun passesSpeakerCheck(preroll: ArrayDeque<ShortArray>): Boolean {
        val embedder = speakerEmbedder
        val enrolled = voiceprint
        if (embedder == null || enrolled == null) return true
        return try {
            val pcm = ShortArray(preroll.sumOf { it.size })
            var offset = 0
            for (chunk in preroll) {
                chunk.copyInto(pcm, offset)
                offset += chunk.size
            }
            val candidate = embedder.embed(pcm)
            if (candidate == null) {
                Log.w(LOG_TAG, "wake speaker check: embed() returned null for ${pcm.size} samples")
                false
            } else {
                val similarity = cosineSimilarity(candidate, enrolled)
                Log.d(
                    LOG_TAG,
                    "wake speaker check: similarity=$similarity threshold=$SPEAKER_SIMILARITY_THRESHOLD " +
                        "gateEnabled=$WAKE_SPEAKER_GATE_ENABLED"
                )
                similarity >= SPEAKER_SIMILARITY_THRESHOLD
            }
        } catch (e: Exception) {
            Log.e(LOG_TAG, "wake speaker check threw — treating as a pass", e)
            true
        }
    }

    @SuppressLint("MissingPermission")
    private fun openRecord(): AudioRecord? {
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
        // Intentionally NO AcousticEchoCanceler / NoiseSuppressor on this mic.
        // This AudioRecord feeds the always-on rustpotter detector, and the
        // VOICE_RECOGNITION source is already the clean signal the wake model
        // was trained on — NS/AEC strip the quiet, far-field wake word right
        // out (regression from fe4e1fe). Follow-up self-echo is handled by the
        // settle + drain + 3-tick gate below, not by mangling the capture.
        rec.startRecording()
        return rec
    }

    /** Blocking-fill one exact detector frame; false on a dead read. */
    private fun fillFrame(rec: AudioRecord, frame: ShortArray): Boolean {
        var off = 0
        while (off < frame.size && alive && !micHold) {
            val n = rec.read(frame, off, frame.size - off)
            if (n <= 0) return false
            off += n
        }
        return off == frame.size
    }

    /** Discard whatever accumulated in the mic buffer while we were speaking,
     *  so the reply's own tail never gets transcribed as a command. */
    private fun drain(rec: AudioRecord) {
        val buf = ShortArray(1600)
        while (rec.read(buf, 0, buf.size, AudioRecord.READ_NON_BLOCKING) > 0) {
            // discard
        }
    }

    // ── One wake interaction (command + follow-ups) ─────────────────────────

    private fun interact(rec: AudioRecord, detector: WakeDetector) {
        // One "Hey Axon" starts one conversation. The id is minted here, once,
        // and reused for every turn — follow-ups AND a mid-reply barge-in — so
        // the whole interaction is saved as a single, separately reviewable
        // conversation. The NEXT wake calls interact() again and mints another.
        val sessionId = prefs.newWakeConversationId()
        // Optional, non-authoritative hint built from the previous wake
        // conversation (before we overwrite it below). Attached only to the
        // first agent task of this conversation.
        val hint = buildHint()
        val turns = ArrayList<Pair<String, String>>()

        var first = true
        var firstTask = true // gates the one-time hint; survives barge-in resets
        var lastReply = ""
        // Non-null right after a confirmed barge-in: the next iteration skips
        // the ack/settle/drain (the user is already mid-sentence) and seeds
        // the capture with this pre-roll instead. Cleared once consumed.
        var bargedPreroll: ByteArray? = null
        // Rides onto the next task only — how much of the reply the user
        // actually heard before they cut it off. Cleared once consumed.
        var pendingNote = ""
        while (alive && !micHold) {
            val barging = bargedPreroll != null
            notify(getString(R.string.status_recording))
            VoiceOverlay.setPhase(VoiceOverlay.Phase.LISTENING)
            var ackPhrase = ""
            val watcher: SilenceWatcher
            val wav: ByteArray
            if (barging) {
                // The user was already talking over the reply when the barge
                // confirmed — replaying an ack, or even the settle+drain
                // pause, would just talk over them again. Capture picks up
                // immediately, seeded with what they said before it confirmed,
                // held to the same strict onset a fresh wake gets (not the
                // lenient follow-up bar).
                watcher = SilenceWatcher()
                wav = capture(rec, watcher, preroll = bargedPreroll)
                bargedPreroll = null
            } else {
                // A wake is answered out loud; the follow-up window opens on
                // its soft chime alone — no spoken prompt. Sound.chime is
                // asynchronous, so hold here for the note (plus the settle
                // below) and let the drain clear it rather than have its tail
                // open the capture.
                ackPhrase = if (first) VoicePrompts.randomWakeAck() else ""
                if (first) {
                    playAckBlocking(ackPhrase, promptFiles[ackPhrase])
                } else {
                    Sound.chime(soft = true)
                    Thread.sleep(250)
                }
                // Let the ack (and any reply tail still in the output pipeline)
                // finish coming out of the speaker before the drain, so it can't
                // leak into the capture and be transcribed as a command. Only in
                // the follow-up window — after a wake the user may already be
                // mid-command and every drained ms is their speech.
                if (!first) Thread.sleep(AUDIO_SETTLE_MS)
                drain(rec)
                watcher = if (first) {
                    SilenceWatcher()
                } else {
                    // The onset requirement is the shared default now; the
                    // follow-up window only still raises the level bar.
                    SilenceWatcher(speechRms = SilenceWatcher.FOLLOWUP_RMS)
                }
                wav = capture(rec, watcher)
            }
            if (!watcher.hadSpeech) break

            notify(getString(R.string.status_thinking))
            VoiceOverlay.setPhase(VoiceOverlay.Phase.THINKING)
            val text = runCatching { client.transcribe(wav) }.getOrNull()
            if (text.isNullOrBlank()) break
            // A capture that is just our own voice bounced back (ack phrase or
            // a fragment of the reply we just spoke) must not become the next
            // command — with session history the agent would happily re-answer
            // the previous question in new words, looping the reply. Skipped
            // entirely on a barge-in: the half-spoken reply the user just cut
            // off is not a safe reference (they may be quoting it back on
            // purpose — "wait, you said X").
            if (!barging && isSelfEcho(text, lastReply, ackPhrase)) break
            // The accepted command is part of THIS wake conversation from here:
            // saved under [sessionId] (its own reviewable thread), and mirrored
            // into the Chat page only if that page is showing this same thread.
            ChatFeed.post(this, sessionId, "user", text)
            // The clean spoken words are what gets saved; the previous-conversation
            // hint (first task only) and any barge-in interruption note ride
            // along to the agent but never into the saved user message. Both
            // are framed as reference-only, so a genuinely new topic ignores them.
            val taskForAgent = (if (firstTask && hint.isNotEmpty()) hint else "") + pendingNote + text
            firstTask = false
            pendingNote = ""
            // Stream the reply: tokens flow into StreamingTts as they arrive,
            // so the first sentence plays ~1s after the agent starts replying
            // instead of after the whole reply is synthesized. Blocks until
            // playback finishes or a barge-in cuts it off.
            val outcome = awaitStreamBlocking(taskForAgent, sessionId, rec, detector)
            val barged = outcome.barged
            val (reply, err) = synchronized(replyLock) { (replyText ?: "") to replyError }
            if (reply.isNotBlank()) {
                ChatFeed.post(this, sessionId, "assistant", reply)
            } else if (!barged && err != null) {
                ChatFeed.post(this, sessionId, "error", "Sorry — $err")
            }
            if (reply.isBlank() && !barged) break
            if (barged) {
                // The user interrupted mid-reply — by talking over it or by
                // saying "Hey Axon"; the barge monitor already ducked/stopped
                // the TTS and cancelled the run. Re-listen for the follow-up in
                // the SAME conversation (sessionId unchanged). This is not a
                // new conversation, so firstTask stays false (no re-hint).
                bargedPreroll = outcome.preroll
                pendingNote = if (outcome.spokenSoFar.isNotBlank()) {
                    "(Note: I interrupted your previous reply mid-speech; I heard only up to: \"${clip(outcome.spokenSoFar)}\") "
                } else {
                    "(Note: I interrupted your previous reply before you said anything.) "
                }
                lastReply = ""
                continue
            }
            turns.add(text to reply)
            lastReply = reply
            first = false // reopen as the follow-up window, raised speech bar
        }
        // Carry this conversation's last two completed turns forward as the next
        // wake's optional hint.
        previousTurns.clear()
        previousTurns.addAll(turns.takeLast(2))
        notify(getString(R.string.notif_listening))
        VoiceOverlay.setPhase(VoiceOverlay.Phase.IDLE)
    }

    /** An optional, deliberately non-authoritative reminder of the previous
     *  wake conversation's last turns. Prepended to the first agent task of a
     *  new conversation so the user can refer back ("what did you say about
     *  that?") without it becoming loaded history that biases an unrelated new
     *  request — the framing tells the agent to ignore it unless relevant. */
    private fun buildHint(): String {
        if (previousTurns.isEmpty()) return ""
        val sb = StringBuilder()
        sb.append("(Reference only — background from our previous spoken conversation. ")
        sb.append("This is a NEW conversation; ignore the following unless what I ask next refers back to it.\n")
        for ((u, a) in previousTurns) {
            sb.append("- I said: \"").append(clip(u)).append("\"; you replied: \"").append(clip(a)).append("\"\n")
        }
        sb.append(")\n\n")
        return sb.toString()
    }

    /** One-line, length-capped copy of a turn for the hint block. */
    private fun clip(s: String, max: Int = 200): String {
        val t = s.trim().replace(Regex("\\s+"), " ")
        return if (t.length <= max) t else t.take(max).trimEnd() + "…"
    }

    /** True when [text] is (a fragment of) what the assistant itself just said
     *  through the speaker: the ack phrase or the reply. Word-subset matching
     *  because STT of an echo tail returns partial, punctuation-free text. */
    private fun isSelfEcho(text: String, lastReply: String, ackWords: String): Boolean {
        fun norm(s: String) = s.lowercase()
            .filter { it.isLetterOrDigit() || it.isWhitespace() }
            .split(Regex("\\s+"))
            .filter { it.isNotEmpty() }

        val words = norm(text)
        if (words.isEmpty()) return true
        val ref = norm("$lastReply $ackWords")
        if (ref.joinToString(" ").contains(words.joinToString(" "))) return true
        return words.size <= 12 && ref.containsAll(words)
    }

    /** Captures a command from [rec], watched by [watcher]. When [preroll] is
     *  supplied (a confirmed barge-in), it's written to the front of the
     *  returned clip and its own RMS is fed through [watcher] first — the
     *  pre-roll covers what the user said in the ~300-600ms it took the
     *  barge-in to confirm, so [watcher] sees it as speech before a single
     *  live frame is read, exactly as if it had been captured live. */
    private fun capture(rec: AudioRecord, watcher: SilenceWatcher, preroll: ByteArray? = null): ByteArray {
        val out = ByteArrayOutputStream()
        val buf = ShortArray(WavRecorder.SAMPLE_RATE / 10)
        if (preroll != null && preroll.isNotEmpty()) {
            out.write(preroll)
            feedPrerollTicks(preroll, watcher)
        }
        while (alive && !micHold) {
            val n = rec.read(buf, 0, buf.size)
            if (n <= 0) break
            val bytes = ByteBuffer.allocate(n * 2).order(ByteOrder.LITTLE_ENDIAN)
            var acc = 0.0
            for (i in 0 until n) {
                bytes.putShort(buf[i])
                val s = buf[i] / 32768.0
                acc += s * s
            }
            out.write(bytes.array(), 0, n * 2)
            val rms = sqrt(acc / n)
            VoiceOverlay.level(rms.toFloat())
            if (watcher.tick(rms)) break
        }
        return WavRecorder.wavBytes(out.toByteArray())
    }

    /** Replays [preroll] (raw 16k mono PCM16) through [watcher] in the same
     *  ~100ms ticks [BargeMonitor] used to produce it, so hadSpeech reflects
     *  the pre-barge audio too. */
    private fun feedPrerollTicks(preroll: ByteArray, watcher: SilenceWatcher) {
        val tickBytes = (WavRecorder.SAMPLE_RATE / 10) * 2 // 100ms of 16-bit mono
        var off = 0
        while (off < preroll.size) {
            val pairs = minOf(tickBytes, preroll.size - off) / 2 // whole samples in this tick
            if (pairs == 0) break
            var acc = 0.0
            for (k in 0 until pairs) {
                val i = off + k * 2
                val s = ((preroll[i].toInt() and 0xff) or (preroll[i + 1].toInt() shl 8)).toShort()
                val f = s / 32768.0
                acc += f * f
            }
            watcher.tick(sqrt(acc / pairs))
            off += pairs * 2
        }
    }

    // ── Speech in/out helpers ───────────────────────────────────────────────

    /** Speak [phrase] using the same 3-tier "never silent" chain as a reply:
     *  prefetched server TTS -> built-in Android TTS -> synthesized chime.
     *  Mirrors voiceprompts.js playPrompt, which always resolves to sound. */
    private fun playAckBlocking(phrase: String, cachedFile: File?) {
        val p = player ?: return
        if (cachedFile != null && cachedFile.exists() && cachedFile.length() > 0) {
            val latch = CountDownLatch(1)
            p.play(cachedFile) { latch.countDown() }
            if (latch.await(4, TimeUnit.SECONDS)) return
        }
        // Server TTS unavailable or stalled — try the built-in engine.
        val spokeLatch = CountDownLatch(1)
        p.speakFallback(phrase) { spokeLatch.countDown() }
        if (spokeLatch.await(4, TimeUnit.SECONDS)) return
        // Last resort: the chime is always available.
        Sound.chime()
        Thread.sleep(400)
    }

    /** Outcome of one streamed reply. [preroll] and [spokenSoFar] are only
     *  meaningful when [barged] is true: the raw mic audio captured in the
     *  ~300-600ms it took to confirm the interruption (so a real-time capture
     *  doesn't lose the user's first words), and how much of the reply they
     *  actually heard before it was cut off (for the follow-up task's
     *  interruption note). */
    private class BargeOutcome(
        val barged: Boolean,
        val preroll: ByteArray = ByteArray(0),
        val spokenSoFar: String = "",
    )

    /**
     * Send [task] and stream the reply: tokens are fed to a [StreamingTts] as
     * they arrive so per-sentence TTS begins with the first sentence, not after
     * the whole reply is downloaded. Watches the mic the whole time for a
     * barge-in — either the wake word, or the user just talking over the
     * reply (see [BargeDetector], which tells the two apart from the echo) —
     * and ducks/stops the reply accordingly.
     *
     * The [BargeMonitor] runs on a throwaway thread that owns the shared mic
     * ([rec]) and rustpotter [wakeDetector] for the whole call: it's safe
     * because runLoop is parked inside interact()→awaitStreamBlocking() for
     * the entire playback, so neither is ever touched concurrently from two
     * threads.
     */
    private fun awaitStreamBlocking(
        task: String,
        sessionId: String,
        rec: AudioRecord?,
        wakeDetector: WakeDetector?,
    ): BargeOutcome {
        val p = player ?: return BargeOutcome(false)
        val c = chat ?: return BargeOutcome(false)
        var waits = 0
        while (!c.connected && waits++ < 10 && alive) Thread.sleep(500)

        notify(getString(R.string.status_speaking))
        VoiceOverlay.setPhase(VoiceOverlay.Phase.SPEAKING)
        val latch = CountDownLatch(1)
        val outcome = java.util.concurrent.atomic.AtomicReference(BargeOutcome(false))
        val stream = StreamingTts(
            player = p,
            client = client,
            cacheDir = cacheDir,
            filePrefix = "reply_wake",
        ) { latch.countDown() }
        synchronized(replyLock) {
            replyLatch = latch
            replyText = null
            replyError = null
            replyStream = stream
        }
        if (!c.sendTask(task, sessionId, voice = true)) {
            stream.abort()
            synchronized(replyLock) { replyStream = null }
            return BargeOutcome(false)
        }
        // Fresh reply: forget the last one's ducked/tentative state, but keep
        // the learned echo gain — it's still the same device and room.
        bargeDetector.reset()
        // Monitor for a barge-in while the reply streams. Only with a live mic
        // — otherwise there's nothing to listen with and we just wait for done.
        val monitor = if (rec != null) {
            thread(name = "axon-barge") {
                BargeMonitor(
                    detector = bargeDetector,
                    wakeDetector = wakeDetector,
                    readFrame = { f -> fillFrame(rec, f) },
                    onTentative = { p.duck() },
                    onFalseAlarm = { p.restoreVolume() },
                    verifySpeaker = speakerVerifier(speakerEmbedder, voiceprint, prefs.bargeMatchThreshold),
                    onConfirmed = { preroll ->
                        val spoken = stream.spokenSoFar()
                        stream.abort() // cut the TTS mid-sentence
                        c.cancel(sessionId) // stop generating, not just talking
                        outcome.set(BargeOutcome(true, preroll, spoken))
                        latch.countDown()
                    },
                ).run { !alive || latch.count == 0L }
            }
        } else null
        latch.await(310, TimeUnit.SECONDS)
        monitor?.join(500)
        val result = outcome.get()
        if (result.barged) drain(rec!!)
        return result
    }

    private fun prefetchPrompts() {
        // One stable file per phrase so existing good fetches survive across
        // prefetch runs (and service restarts); only missing/empty ones fetch.
        // SHA-1 of the phrase keeps filenames stable and collision-free.
        for (phrase in VoicePrompts.allPrefetchable) {
            if (promptFiles[phrase]?.let { it.exists() && it.length() > 0 } == true) continue
            val file = File(cacheDir, "ack_${phrase.hashCode().toString(16)}.audio")
            val ok = file.exists() && file.length() > 0 ||
                runCatching { client.speech(phrase, file) }.getOrDefault(false)
            if (ok && file.length() > 0) {
                promptFiles[phrase] = file
            } else {
                file.delete() // retry on the next prefetch / service start
            }
        }
    }

    // ── Chat plumbing ───────────────────────────────────────────────────────

    override fun onWsEvent(ev: JSONObject) {
        when (ev.optString("type")) {
            // Streamed reply token — feed it straight to TTS so speech begins
            // with the first sentence, not after the whole reply arrives.
            "token" -> synchronized(replyLock) { replyStream?.append(ev.optString("text")) }
            "done" -> {
                val full = ev.optString("full_text", "")
                // done may arrive before the tokens have drained through TTS;
                // if we somehow got full_text with no tokens streamed (e.g. a
                // server that doesn't emit token frames), synthesize it now as
                // one fallback chunk so we're never silent.
                val stream = synchronized(replyLock) {
                    replyText = full
                    val s = replyStream
                    replyStream = null
                    s
                }
                if (stream != null && stream.hasContent) {
                    // Streaming reply: let it play out. finish() only schedules
                    // the finalize behind any in-flight synths — it does NOT
                    // block, so we must NOT count the latch down here. The
                    // StreamingTts onDone callback (wired to latch.countDown)
                    // fires once playback truly drains, and that is the only
                    // path that releases the caller. Counting down here would
                    // release interact() before the reply finished playing, and
                    // its next ack's TtsPlayer.play()→stop() would cut the
                    // reply's TTS off — the reply was never heard, only the ack.
                    stream.finish()
                    return
                }
                // The stream exists but never received a token frame. Finalizing
                // it would speak nothing at all, so drop it and synthesize
                // full_text below instead.
                stream?.abort()
                if (full.isNotBlank()) {
                    // Synthesize the whole reply as one fallback chunk so we're
                    // never silent.
                    // Off this thread: it's OkHttp's WS reader, and a slow
                    // synthesis here would stall pings and drop the socket.
                    thread(name = "axon-fallback-tts") {
                        val p = player
                        val f = File(cacheDir, "reply_wake.audio")
                        if (p != null && client.speech(full, f) && f.length() > 0) {
                            p.play(f) {
                                synchronized(replyLock) {
                                    replyLatch?.countDown()
                                    replyLatch = null
                                }
                            }
                        } else {
                            synchronized(replyLock) {
                                replyLatch?.countDown()
                                replyLatch = null
                            }
                        }
                    }
                    return
                }
                // Empty reply, no stream — nothing to speak. Release the caller
                // so it doesn't hang for the full 310s latch timeout.
                synchronized(replyLock) {
                    replyLatch?.countDown()
                    replyLatch = null
                }
            }
            "error" -> {
                synchronized(replyLock) {
                    replyError = ev.optString("message", "something went wrong")
                    replyStream?.abort()
                    replyStream = null
                }
                player?.stop()
                synchronized(replyLock) {
                    replyLatch?.countDown()
                    replyLatch = null
                }
            }
        }
    }

    // ── Notification ────────────────────────────────────────────────────────

    private fun createChannel() {
        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(
            NotificationChannel(
                CHANNEL,
                getString(R.string.notif_channel_wake),
                NotificationManager.IMPORTANCE_LOW
            )
        )
    }

    private fun notif(text: String): Notification {
        val open = PendingIntent.getActivity(
            this, 0,
            Intent(this, ChatActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE
        )
        val stop = PendingIntent.getService(
            this, 1,
            Intent(this, WakeWordService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_IMMUTABLE
        )
        return Notification.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_mic)
            .setContentTitle(getString(R.string.app_name))
            .setContentText(text)
            .setContentIntent(open)
            .setOngoing(true)
            .addAction(
                Notification.Action.Builder(
                    Icon.createWithResource(this, R.drawable.ic_mic), "Turn off", stop
                ).build()
            )
            .build()
    }

    private fun notify(text: String) {
        getSystemService(NotificationManager::class.java).notify(NOTIF_ID, notif(text))
    }

    private fun fail(msg: String) {
        notify(msg)
        if (Build.VERSION.SDK_INT >= 24) {
            stopForeground(STOP_FOREGROUND_DETACH)
        }
        stopSelf()
    }
}
