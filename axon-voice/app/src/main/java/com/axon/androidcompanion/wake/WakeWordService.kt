package com.axon.androidcompanion.wake

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
import com.axon.androidcompanion.Prefs
import com.axon.androidcompanion.R
import com.axon.androidcompanion.api.AxonClient
import com.axon.androidcompanion.api.ChatSocket
import com.axon.androidcompanion.audio.Sound
import com.axon.androidcompanion.audio.SilenceWatcher
import com.axon.androidcompanion.audio.StreamingTts
import com.axon.androidcompanion.audio.TtsPlayer
import com.axon.androidcompanion.audio.VoicePrompts
import com.axon.androidcompanion.audio.WavRecorder
import com.axon.androidcompanion.ui.ChatFeed
import com.axon.androidcompanion.ui.ChatActivity
import com.axon.androidcompanion.ui.VoiceOverlay
import org.json.JSONObject
import java.io.ByteArrayOutputStream
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.ArrayDeque
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
        const val ACTION_STOP = "com.axon.androidcompanion.STOP_WAKE"

        /** MediaPlayer's completion callback can fire a few hundred ms before
         *  the audio sink actually finishes playing out (worse on Bluetooth).
         *  Waiting this long before the pre-capture drain keeps our own ack /
         *  reply tail out of the follow-up capture. */
        private const val AUDIO_SETTLE_MS = 250L

        /** Consecutive loud ~30ms frames the barge monitor needs before it
         *  pauses the reply — ~120ms, long enough to ignore a click/knock,
         *  short enough to catch a word onset. */
        private const val BARGE_ONSET_FRAMES = 4

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

    /** True while a reply's barge monitor thread should keep watching the mic;
     *  cleared once the reply's latch releases so the monitor unwinds. Only one
     *  reply is ever in flight (single wake worker), so one flag suffices. */
    @Volatile
    private var bargeMonitorAlive = false

    /** Orb-core tap hold. While set: the reply stream is paused (no new sentence
     *  plays) and the command/follow-up [capture] loop freezes (mic drained but
     *  not accumulated or timed out), so one toggle holds whichever half of the
     *  exchange is live. Cleared on resume or [closeExchange]. */
    @Volatile
    private var paused = false

    /** Set by [closeExchange] (tap outside the orb) to end the current exchange:
     *  [interact]'s loops break out and the service falls back to passive wake
     *  listening. Reset at the top of each [interact]. */
    @Volatile
    private var cancelExchange = false

    /** The orb's two gestures, routed here from [ChatActivity] via
     *  [VoiceOverlay.controller]. The service owns the reply stream and capture
     *  loop, so it does the actual hold/resume and close. */
    private val overlayController = object : VoiceOverlay.Controller {
        override fun onTogglePause() = togglePause()
        override fun onClose() = closeExchange()
    }

    /** run_id of the reply whose events we're currently accepting (every
     *  AgentEvent carries one). Updated as events arrive; read on a barge to
     *  abandon the interrupted run. */
    @Volatile
    private var lastRunId: String? = null

    /** run_ids of runs superseded by a barge. A barge starts a NEW task on the
     *  same session, which the server auto-supersedes the old run for — but the
     *  old run's in-flight tokens (and any late terminal event) keep arriving on
     *  the socket; ignoring them by run_id keeps the interrupted reply's tail
     *  from polluting or prematurely ending the new one. Cleared per wake
     *  conversation in [interact]. */
    private val abandonedRuns = java.util.Collections.synchronizedSet(HashSet<String>())

    /** Filled by the barge monitor when a real spoken interruption is confirmed
     *  during a reply: [command] is the transcribed interruption (the next
     *  turn), [partial] is however much of the cut-off reply had been spoken. */
    private class BargeSlot {
        @Volatile
        var command: String? = null

        @Volatile
        var partial: String = ""
    }

    /** The last up-to-2 completed turns of the *previous* wake conversation,
     *  offered as an optional hint (not loaded history) at the start of the
     *  next one — so a fresh "Hey Axon" can pick up a thread if the user refers
     *  back, but starts clean otherwise. Only ever touched from the wake
     *  thread's interact(), so it needs no lock. */
    private val previousTurns = ArrayList<Pair<String, String>>()

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        prefs = Prefs(this)
        client = AxonClient(prefs)
        // The reply-audio RMS drives the SPEAKING orb in realtime; speakLevel
        // ignores it outside the speaking phase so ack playback won't leak into
        // the orb.
        player = TtsPlayer(this).apply {
            onLevel = { rms -> onReplyLevel(rms) }
        }
        VoiceOverlay.controller = overlayController
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopSelf()
            return START_NOT_STICKY
        }
        createChannel()
        startForeground(NOTIF_ID, notif(getString(R.string.notif_listening, wakeLabel())))
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
        if (VoiceOverlay.controller === overlayController) VoiceOverlay.controller = null
        setPhase(VoiceOverlay.Phase.IDLE)
        chat?.close()
        chat = null
        player?.release()
        player = null
        wakeLock?.release()
        wakeLock = null
        worker?.join(1500)
        worker = null
        super.onDestroy()
    }

    // ── Detection loop ──────────────────────────────────────────────────────

    /** The enrolled wake phrase, or the "Hey Axon" default label if somehow
     *  empty despite enrollment (cosmetic only — doesn't affect which model
     *  loads; see [runLoop]). */
    private fun wakeLabel(): String = prefs.wakePhrase.ifEmpty { "Hey Axon" }

    private fun runLoop() {
        if (!WakeDetector.available) {
            fail("Wake engine missing from this build")
            return
        }
        // Callers only ever start this service once prefs.wakeEnrolled is true
        // (see ChatActivity.setWakeEnabled) — there is no shared/default model
        // to fall back to anymore, enrollment is required.
        val model = try {
            File(prefs.wakeModelPath).readBytes()
        } catch (_: Exception) {
            fail("Wake model missing — re-enroll your wake word")
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
                val score = detector.process(frame)
                if (score >= 0f) {
                    interact(rec)
                    drain(rec)
                }
            }
        } finally {
            record?.release()
            detector.close()
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
        // It feeds the always-on rustpotter detector, and the
        // VOICE_RECOGNITION source is already the clean signal the wake model
        // was trained on — NS/AEC strip the quiet, far-field wake word right
        // out (regression from fe4e1fe). Follow-up self-echo is handled by
        // the settle + drain + 3-tick gate below, not by mangling the capture.
        try {
            rec.startRecording()
        } catch (_: Exception) {
            rec.release()
            return null
        }
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

    // ── Orb gestures (pause / close) ────────────────────────────────────────

    /** Orb-core tap: hold or resume whichever half of the exchange is live. A
     *  playing reply pauses in place (its stream re-queues the current sentence
     *  for replay on resume); an open [capture] window freezes on its [paused]
     *  check. Idle phases (thinking with nothing queued yet) just arm the hold
     *  so the next spoken sentence waits for a resume. */
    private fun togglePause() {
        paused = !paused
        val s = synchronized(replyLock) { replyStream }
        if (paused) s?.pause() else s?.resume()
        VoiceOverlay.setPaused(paused)
    }

    /** Tap outside the orb: end this exchange. Unblocks whatever [interact] is
     *  waiting on (an in-flight reply's latch, the capture loop) so it breaks
     *  out and the wake loop returns to passive "Hey Axon" listening. The wake
     *  service itself keeps running. */
    private fun closeExchange() {
        cancelExchange = true
        bargeMonitorAlive = false
        if (paused) {
            paused = false
            VoiceOverlay.setPaused(false)
        }
        synchronized(replyLock) {
            replyStream?.abort()
            replyStream = null
            replyLatch?.countDown()
            replyLatch = null
        }
        player?.stop()
    }

    // ── One wake interaction (command + follow-ups) ─────────────────────────

    private fun interact(rec: AudioRecord) {
        cancelExchange = false
        if (paused) {
            paused = false
            VoiceOverlay.setPaused(false)
        }
        // One "Hey Axon" starts one conversation. The id is minted here, once,
        // and reused for every follow-up turn — so the whole interaction is
        // saved as a single, separately reviewable conversation. The NEXT wake
        // calls interact() again and mints another.
        val sessionId = prefs.newWakeConversationId()
        abandonedRuns.clear() // fresh conversation — no superseded runs to ignore yet
        // Optional, non-authoritative hint built from the previous wake
        // conversation (before we overwrite it below). Attached only to the
        // first agent task of this conversation.
        val hint = buildHint()
        val turns = ArrayList<Pair<String, String>>()

        var first = true
        var firstTask = true // gates the one-time hint
        var lastReply = ""
        // A spoken interruption confirmed while the last reply played: it IS the
        // next command, so the loop below skips the ack + mic capture and feeds
        // it straight to the agent. Null on a normal turn.
        var pending: String? = null
        while (alive && !micHold && !cancelExchange) {
            val text: String
            val ackPhrase: String
            if (pending != null) {
                // Barge-in: the user already talked over the reply, so there's
                // nothing to listen for — no ack, no capture. The words were
                // transcribed with the reply PAUSED (echo-free), so they're
                // clean; use them as the command directly.
                text = pending!!
                pending = null
                ackPhrase = ""
                ChatFeed.post(this, sessionId, "user", text)
            } else {
                notify(getString(R.string.status_recording))
                setPhase(VoiceOverlay.Phase.LISTENING)
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
                val watcher = if (first) {
                    SilenceWatcher()
                } else {
                    // The onset requirement is the shared default now; the
                    // follow-up window still raises the level bar, and its length
                    // (how long to wait for the user to start answering) is a
                    // user setting so it doesn't close before they respond.
                    SilenceWatcher(
                        speechRms = SilenceWatcher.FOLLOWUP_RMS,
                        noSpeechTicks = prefs.followupWindowTicks,
                    )
                }
                val wav = capture(rec, watcher)
                if (!watcher.hadSpeech) break

                notify(getString(R.string.status_thinking))
                setPhase(VoiceOverlay.Phase.THINKING)
                val heard = runCatching { client.transcribe(wav) }.getOrNull()
                if (heard.isNullOrBlank()) break
                // A capture that is just our own voice bounced back (ack phrase
                // or the last full reply) must not become the next command: with
                // session history the agent would re-answer its own words,
                // looping the reply.
                if (isSelfEcho(heard, lastReply, ackPhrase)) break
                // The accepted command is part of THIS wake conversation from
                // here: saved under [sessionId] (its own reviewable thread), and
                // mirrored into the Chat page only if that page is showing this
                // same thread.
                ChatFeed.post(this, sessionId, "user", heard)
                text = heard
            }
            // The clean spoken words are what gets saved; the previous-conversation
            // hint (first task only) rides along to the agent but never into the
            // saved user message — framed as reference-only, so a genuinely new
            // topic ignores it.
            val taskForAgent = (if (firstTask && hint.isNotEmpty()) hint else "") + text
            firstTask = false
            // Thinking until the reply is audible (covers the barge/pending turn,
            // which skipped the capture branch that would have set it). The flip
            // to SPEAKING happens in onReplyLevel at the first audible frame.
            notify(getString(R.string.status_thinking))
            setPhase(VoiceOverlay.Phase.THINKING)
            // Stream the reply: tokens flow into StreamingTts as they arrive, so
            // the first sentence plays ~1s after the agent starts replying
            // instead of after the whole reply is synthesized. Blocks until
            // playback finishes — or until the user barges in, which returns the
            // interrupting command to run as the next turn.
            val barge = awaitStreamBlocking(taskForAgent, sessionId, rec)
            val (reply, err) = synchronized(replyLock) { (replyText ?: "") to replyError }
            // On a barge the reply was cut off before "done" arrived, so save the
            // partial that had actually been spoken; otherwise the full reply.
            val spoken = when {
                reply.isNotBlank() -> reply
                barge != null -> barge.partial.trim()
                else -> ""
            }
            if (spoken.isNotBlank()) {
                ChatFeed.post(this, sessionId, "assistant", spoken)
                turns.add(text to spoken)
                lastReply = spoken
            } else if (err != null) {
                ChatFeed.post(this, sessionId, "error", "Sorry — $err")
            }
            first = false // reopen as the follow-up window, raised speech bar
            if (barge != null) {
                // Run the interruption as the next turn: no ack, no re-listen.
                pending = barge.command
                continue
            }
            if (spoken.isBlank()) break
        }
        // Carry this conversation's last two completed turns forward as the next
        // wake's optional hint.
        previousTurns.clear()
        previousTurns.addAll(turns.takeLast(2))
        notify(getString(R.string.notif_listening, wakeLabel()))
        setPhase(VoiceOverlay.Phase.IDLE)
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

    /** Captures a command from [rec], watched by [watcher]. A [paused] hold
     *  freezes the window: the mic is still drained (so a resume doesn't replay
     *  buffered audio) but nothing is accumulated or ticked, so the watcher
     *  can't time out and the clip stays paused until resume or [cancelExchange]. */
    private fun capture(rec: AudioRecord, watcher: SilenceWatcher): ByteArray {
        val out = ByteArrayOutputStream()
        val buf = ShortArray(WavRecorder.SAMPLE_RATE / 10)
        while (alive && !micHold && !cancelExchange) {
            val n = rec.read(buf, 0, buf.size)
            if (n <= 0) break
            if (paused) continue
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

    // ── Speech in/out helpers ───────────────────────────────────────────────

    /** Reply-audio level from the TTS player. The first audible frame — a real
     *  (non-negative) RMS arriving while we're still in THINKING — is the true
     *  start of SPEAKING, so the phase flips HERE, at the DAC, not when the task
     *  was sent. Between sending the task and the first sound sit an agent-think
     *  and a first-sentence synth round-trip; flipping up front made the label
     *  lead the voice by that whole gap. Inter-sentence gaps (a lone -1) don't
     *  revert it — the reply stays SPEAKING until it truly ends. */
    private fun onReplyLevel(rms: Float) {
        if (rms >= 0f && VoiceOverlay.phase == VoiceOverlay.Phase.THINKING) {
            notify(getString(R.string.status_speaking))
            setPhase(VoiceOverlay.Phase.SPEAKING)
        }
        VoiceOverlay.speakLevel(rms)
    }

    /** Set the hands-free phase AND drive the THINKING "still here" shimmer, so
     *  every phase change keeps the cue in step: it plays only while THINKING —
     *  filling the silent think + first-synth gap — and stops the instant the
     *  reply starts speaking or the turn ends. */
    private fun setPhase(p: VoiceOverlay.Phase) {
        if (p == VoiceOverlay.Phase.THINKING) Sound.startThinking() else Sound.stopThinking()
        VoiceOverlay.setPhase(p)
    }

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

    /**
     * Send [task] and stream the reply: tokens are fed to a [StreamingTts] as
     * they arrive so per-sentence TTS begins with the first sentence, not after
     * the whole reply is downloaded. Blocks until playback finishes.
     *
     * When barge-in is on and [rec] is available, a monitor thread watches the
     * mic's low band while the reply plays; a confirmed spoken interruption cuts
     * the reply short and this returns a [BargeSlot] whose [BargeSlot.command] is
     * the interruption to run next. Returns null on a normal, uninterrupted
     * reply.
     */
    private fun awaitStreamBlocking(
        task: String,
        sessionId: String,
        rec: AudioRecord?,
    ): BargeSlot? {
        val p = player ?: return null
        val c = chat ?: return null
        var waits = 0
        while (!c.connected && waits++ < 10 && alive) Thread.sleep(500)

        // Deliberately DON'T flip to SPEAKING here: the reply is still an
        // agent-think plus a first-sentence synth round-trip away from any
        // sound. [onReplyLevel] moves to SPEAKING when the first frame is
        // actually audible (at the DAC), so the label tracks the voice instead
        // of leading it. The caller left us in THINKING, which is accurate until
        // then.
        val latch = CountDownLatch(1)
        lateinit var stream: StreamingTts
        stream = StreamingTts(
            player = p,
            client = client,
            cacheDir = cacheDir,
            filePrefix = "reply_wake",
        ) {
            // Playback has truly drained (not just text generation finishing —
            // see the "done" handler below, which deliberately leaves
            // [replyStream] live so a pause tapped after the WS "done" event
            // but while trailing sentences are still speaking still reaches
            // this stream). Only clear it here, at the point pause/resume have
            // nothing left to act on, and only if a newer reply hasn't already
            // replaced it (a barge's follow-up task, started while this one's
            // tail was still draining).
            synchronized(replyLock) { if (replyStream === stream) replyStream = null }
            latch.countDown()
        }
        synchronized(replyLock) {
            replyLatch = latch
            replyText = null
            replyError = null
            replyStream = stream
        }
        if (!c.sendTask(task, sessionId, voice = true)) {
            stream.abort()
            synchronized(replyLock) { replyStream = null }
            return null
        }

        // Barge-in only runs with a headset: the reply then plays into the
        // earpiece, not the loudspeaker, so there is NO echo path back into the
        // mic and a plain loudness trigger is safe. On the built-in speaker the
        // echo swamps the mic and no single-mic trigger can separate it from the
        // user (proven on-device — see [[barge-in-speaker-verification]]), so the
        // reply just plays to completion there.
        val slot = BargeSlot()
        val monitor = if (prefs.bargeInEnabled && rec != null && hasHeadsetOutput()) {
            bargeMonitorAlive = true
            thread(name = "axon-barge") { monitorBarge(rec, stream, latch, slot) }
        } else {
            null
        }
        latch.await(310, TimeUnit.SECONDS)
        // Stop the monitor (normal completion) and let it unwind; on a barge it
        // has already exited on its own.
        bargeMonitorAlive = false
        monitor?.join(500)
        return if (slot.command != null) slot else null
    }

    /**
     * Watch the mic for a barge-in while a reply plays. Runs on its own thread
     * for the reply's duration, and ONLY when a headset is connected (gated by
     * the caller), so the reply is in the earpiece and never reaches the mic —
     * no echo to fight.
     *
     * With no echo, a plain loudness onset is a safe trigger: the reply can't
     * trip it, and a raised bar keeps distant/quiet voices out. On a sustained
     * loud onset the reply is PAUSED and the clip (a short pre-roll of what
     * triggered it, plus live audio until the user stops) is checked. A clip
     * with no sustained near speech (a clap, a knock) is dropped without even a
     * transcription; a cough that reaches STT comes back empty (the server's
     * silence filter) — either way the reply RESUMES from the interrupted
     * sentence. Real words are a genuine barge: the server run is cancelled, the
     * reply aborted, and the words handed back via [slot] to run as the next turn.
     */
    private fun monitorBarge(
        rec: AudioRecord,
        stream: StreamingTts,
        latch: CountDownLatch,
        slot: BargeSlot,
    ) {
        val onset = prefs.bargeOnsetLevel / 10000.0
        val frameSamples = WavRecorder.SAMPLE_RATE * 30 / 1000 // ~30ms frames
        val frame = ShortArray(frameSamples)
        // ~480ms of recent audio so a fired trigger's clip includes the speech
        // onset that the sustain requirement needed a moment to confirm.
        val preRollFrames = 16
        val preRoll = ArrayDeque<ByteArray>()
        // Drop whatever was buffered before the monitor started.
        drain(rec)
        var loud = 0
        // Don't fire until the reply is actually audible: a slow first-sentence
        // synth leaves the mic open while the speaker is still silent, and firing
        // then would abort the reply before it said anything. Sticky once set, so
        // the pause/resume below doesn't disarm it.
        var armed = false
        var lastDiag = 0L
        while (bargeMonitorAlive && alive && !micHold) {
            if (!fillFrame(rec, frame)) continue
            preRoll.addLast(frameBytes(frame, frameSamples))
            while (preRoll.size > preRollFrames) preRoll.pollFirst()
            if (!armed && player?.playing == true) armed = true

            val rms = frameRms(frame, frameSamples)
            val now = System.currentTimeMillis()
            if (now - lastDiag > 250) {
                lastDiag = now
                val tag = if (armed) "" else " (waiting for play)"
                notify("barge: mic=%.4f thr=%.4f%s".format(rms, onset, tag))
            }

            if (!armed) {
                loud = 0
                continue
            }
            if (rms > onset) loud++ else loud = 0
            if (loud < BARGE_ONSET_FRAMES) continue
            loud = 0

            // Probable barge — pause the reply and capture the utterance. Flip
            // the orb to LISTENING so it reflects "listening to you now", not a
            // frozen SPEAKING, for the duration of the capture.
            stream.pause()
            notify(getString(R.string.status_recording))
            setPhase(VoiceOverlay.Phase.LISTENING)
            val wav = captureBarge(rec, preRoll)
            // No sustained near speech (a clap/knock/blip) — captureBarge returns
            // null; don't transcribe it. A cough reaches STT but comes back empty.
            val text = if (wav != null) runCatching { client.transcribe(wav) }.getOrNull() else null
            if (text.isNullOrBlank()) {
                // Not a real interruption — resume the reply. Back to SPEAKING
                // explicitly: onReplyLevel only flips THINKING->SPEAKING, so the
                // resumed audio wouldn't move the orb off LISTENING on its own.
                preRoll.clear()
                drain(rec)
                stream.resume()
                notify(getString(R.string.status_speaking))
                setPhase(VoiceOverlay.Phase.SPEAKING)
                continue
            }
            // Genuine interruption. Abandon the interrupted run by id so its
            // in-flight tokens and its superseded "done" are ignored from here
            // on, abort local playback, and hand the words + whatever was spoken
            // back to interact. DON'T send an explicit cancel: that makes the
            // server emit an empty "done" that would abort the barge reply — and
            // sending the barge command as a new task already supersedes this run
            // server-side (ws.rs), silently. Release the latch so awaitStream
            // returns.
            slot.partial = stream.accumulated()
            slot.command = text
            lastRunId?.let { abandonedRuns.add(it) }
            synchronized(replyLock) {
                replyStream?.abort()
                replyStream = null
                replyLatch = null
            }
            latch.countDown()
            return
        }
    }

    /** Build a WAV clip for a barge check: the [preRoll] frames (the audio that
     *  tripped the gate) followed by live capture from [rec] until the user
     *  stops. Returns null when the clip holds no sustained near speech — a
     *  clap, a knock, a lone loud blip — so a near-silent clip is never sent to
     *  the transcriber (which would hallucinate a phrase out of it). A raised
     *  speech bar also rejects distant talkers; a short no-speech budget bails
     *  fast on a cough. */
    private fun captureBarge(
        rec: AudioRecord,
        preRoll: ArrayDeque<ByteArray>,
    ): ByteArray? {
        val out = ByteArrayOutputStream()
        for (b in preRoll) out.write(b, 0, b.size)
        val watcher = SilenceWatcher(
            speechRms = SilenceWatcher.FOLLOWUP_RMS,
            noSpeechTicks = 12,
            quietTicks = 12,
        )
        val buf = ShortArray(WavRecorder.SAMPLE_RATE / 10) // ~100ms ticks
        while (alive && !micHold && bargeMonitorAlive) {
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
            VoiceOverlay.level(rms.toFloat()) // drive the LISTENING orb while capturing
            if (watcher.tick(rms)) break
        }
        if (!watcher.hadSpeech) return null
        return WavRecorder.wavBytes(out.toByteArray())
    }

    /** One frame of PCM16 samples as little-endian bytes for the pre-roll. */
    private fun frameBytes(frame: ShortArray, len: Int): ByteArray {
        val bytes = ByteBuffer.allocate(len * 2).order(ByteOrder.LITTLE_ENDIAN)
        for (i in 0 until len) bytes.putShort(frame[i])
        return bytes.array()
    }

    /** Broadband RMS (0..1) of one PCM16 frame — the barge loudness trigger. */
    private fun frameRms(frame: ShortArray, len: Int): Double {
        var acc = 0.0
        for (i in 0 until len) {
            val s = frame[i] / 32768.0
            acc += s * s
        }
        return if (len > 0) sqrt(acc / len) else 0.0
    }

    /** True when the reply's audio is going somewhere other than the phone's own
     *  loudspeaker — a wired / USB / Bluetooth headset — so there is no acoustic
     *  path from the speaker back into the mic. Barge-in only runs in this case;
     *  on the built-in speaker the reply's echo swamps the mic and no single-mic
     *  trigger can tell it apart from the user. */
    private fun hasHeadsetOutput(): Boolean {
        val am = getSystemService(Context.AUDIO_SERVICE) as? android.media.AudioManager ?: return false
        return am.getDevices(android.media.AudioManager.GET_DEVICES_OUTPUTS).any {
            when (it.type) {
                android.media.AudioDeviceInfo.TYPE_WIRED_HEADSET,
                android.media.AudioDeviceInfo.TYPE_WIRED_HEADPHONES,
                android.media.AudioDeviceInfo.TYPE_USB_HEADSET,
                android.media.AudioDeviceInfo.TYPE_BLUETOOTH_A2DP,
                android.media.AudioDeviceInfo.TYPE_BLUETOOTH_SCO,
                -> true
                else -> false
            }
        }
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

    /** A dropped socket won't have its in-flight run redelivered on reconnect —
     *  the server binds a run to the socket it started on — so a reply left
     *  awaiting would otherwise freeze the hands-free orb until the 310s latch
     *  backstop in [awaitStreamBlocking] finally fires. Release it now instead:
     *  OkHttp's 25s pingInterval surfaces even a half-open mobile socket here
     *  within ~25-50s, so [interact] sees a blank reply + error and ends the
     *  exchange promptly (VoiceOverlay back to IDLE). No-op when nothing is
     *  awaiting (idle wake-word listening) — [ChatSocket] just reconnects and
     *  the conversation was never mid-reply to begin with. */
    override fun onWsDisconnected() {
        synchronized(replyLock) {
            if (replyLatch == null) return // idle: nothing to unstick
            if (replyError == null) replyError = "Connection lost before the reply finished."
            replyStream?.abort()
            replyStream = null
        }
        player?.stop()
        synchronized(replyLock) {
            replyLatch?.countDown()
            replyLatch = null
        }
    }

    override fun onWsEvent(ev: JSONObject) {
        // Drop anything from a run a barge already walked away from: its tail
        // tokens and its late/superseded "done" must not touch the reply that
        // replaced it (a stray empty "done" here would abort the new reply's
        // stream and end the conversation). Every AgentEvent carries run_id.
        val runId = ev.optString("run_id")
        if (runId.isNotEmpty()) {
            if (abandonedRuns.contains(runId)) return
            lastRunId = runId
        }
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
                    replyStream
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
                    //
                    // Deliberately leave [replyStream] set rather than nulling it
                    // here: this "done" event only means text generation
                    // finished, and trailing sentences can still be speaking for
                    // a while after. Nulling it now would make a pause/resume
                    // tapped during that window a silent no-op (the orb flips to
                    // "paused" but the audio keeps playing). StreamingTts's
                    // onDone callback (above, in awaitStreamBlocking) clears it
                    // once playback truly drains.
                    stream.finish()
                    return
                }
                // The stream exists but never received a token frame. Finalizing
                // it would speak nothing at all, so drop it and synthesize
                // full_text below instead — nothing was ever audible from it, so
                // there's nothing left for pause/resume/close to reach.
                synchronized(replyLock) { if (replyStream === stream) replyStream = null }
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
