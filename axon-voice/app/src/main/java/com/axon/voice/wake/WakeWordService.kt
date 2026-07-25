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
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.PowerManager
import android.util.Log
import android.widget.Toast
import com.axon.voice.Prefs
import com.axon.voice.R
import com.axon.voice.api.AxonClient
import com.axon.voice.api.ChatSocket
import com.axon.voice.audio.BargeMonitor
import com.axon.voice.audio.Sound
import com.axon.voice.audio.SilenceWatcher
import com.axon.voice.audio.StreamingTts
import com.axon.voice.audio.TtsPlayer
import com.axon.voice.audio.VoicePrompts
import com.axon.voice.audio.WavRecorder
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

        /** Detection cutoff for the barge keyword model ("okay"/"wait", built
         *  from the user's own recordings). Higher than the wake word's 0.47:
         *  offline sweeps showed ~0.5 keeps 100% recall on the user saying the
         *  word while dropping to zero false-fires on both the reply's own voice
         *  and ordinary non-keyword speech. On-device echo lowers real scores,
         *  so if genuine "okay"/"wait" misses, lower this (watch logcat tag
         *  BargeKeyword for live scores). 0.42: on-device, the reply playing
         *  around the phrase lowers real scores, so the clean-recording sweet
         *  spot (0.50) missed everything; the one round that actually fired
         *  on-device used ~0.45, and 0.42 gives margin for a phrase spoken
         *  mid-sentence (masked by the reply). reply_synth self-interrupt stays 0
         *  at this level (the reply is a different voice, can't match the user's
         *  templates). Lower toward 0.38 if phrases still miss on-device. */
        private const val BARGE_THRESHOLD = 0.42f

        /** Averaged-score gate ([WakeDetector.avgThreshold]). MUST stay 0 (off):
         *  it requires the detection window's MEAN score to clear it, but
         *  on-device the window is polluted by the reply audio around the phrase,
         *  which drags the mean down and SUPPRESSED all detection — recall fell
         *  monotonically as this rose (0.0 fired, 0.1 "need to pause", 0.2 never).
         *  The clean-offline sweep couldn't see this (no reply in the takes).
         *  Precision instead comes from the single-frame threshold + the phrases'
         *  own specificity; a stray fire is harmless (the barge just ends the turn
         *  cleanly, no loop). */
        private const val BARGE_AVG_THRESHOLD = 0.0f

        /** Barge-phrase model asset(s), loaded explicitly (see [loadBargeModels]
         *  — do NOT rely on assets.list). ALL phrases live in ONE model file
         *  ([barge.rpw], built from every phrase's recordings) so a SINGLE
         *  rustpotter engine extracts audio features once per frame and matches
         *  them against all phrase templates — the topology that worked on-device.
         *  Seven SEPARATE per-phrase engines (one recomputing features each) both
         *  wasted CPU and stopped detecting anything on-device; a combined model
         *  fires on any phrase far more cheaply. To add a phrase: rebuild the
         *  combined model from all phrases' recordings and replace barge.rpw. */
        private val BARGE_MODELS = listOf("barge.rpw")

        /** Genuine silence [TtsPlayer.interChunkGapMs] holds between queued
         *  reply chunks during a barge-monitored reply — the ACTUAL fix (see
         *  the long comment in [awaitStreamBlocking]): offline measurement
         *  showed keyword recall is 18/18 in true silence and floors out the
         *  instant any reply audio overlaps a phrase at all, so the win comes
         *  from manufacturing frequent, real silent windows (paired with
         *  [StreamingTts.fineChunking]'s clause-level splitting), not from
         *  threshold tuning or echo cancellation alone. 800ms comfortably
         *  covers the ~0.5-1.2s of actual speech inside these phrases'
         *  ~1.5-2s recordings (measured from the offline test-kit wavs) while
         *  still reading as a natural conversational pause, not a stutter. */
        private const val BARGE_INTER_CHUNK_GAP_MS = 800

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

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        prefs = Prefs(this)
        client = AxonClient(prefs)
        // The reply-audio RMS drives the SPEAKING orb in realtime; speakLevel
        // ignores it outside the speaking phase so ack playback won't leak into
        // the orb. Barge-in no longer needs a playback reference — it's keyword
        // spotting now, not an echo/energy threshold.
        player = TtsPlayer(this).apply {
            onLevel = { rms -> VoiceOverlay.speakLevel(rms) }
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
        // Barge keyword models — every `barge*.rpw` asset, one detector each,
        // each built from the user's own voice for a distinct interrupt phrase
        // ("okay wait", "excuse me", "teka lang", …). Run alongside the wake word
        // only while a reply is speaking; a hit on ANY confirms the barge-in.
        // More distinct phrases = more chances to catch a real interrupt. The
        // wake word ("hey axon") already doubles as an interrupt phrase for free.
        // Long-lived like the wake detector; all closed in finally.
        val bargeWakes: List<WakeDetector> = loadBargeModels(detector.samplesPerFrame)
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
                    interact(rec, detector, bargeWakes)
                    drain(rec)
                }
            }
        } finally {
            record?.release()
            detector.close()
            bargeWakes.forEach { it.close() }
        }
    }

    /** Load a [WakeDetector] for each barge-phrase asset — one interrupt phrase
     *  each. Skips (and closes) any whose frame size doesn't match the wake
     *  model's; an empty set just means barge-in falls back to the wake word.
     *
     *  Discovery is by an EXPLICIT filename list ([BARGE_MODELS]) opened the same
     *  proven way the wake model is (`assets.open(name)`), NOT `assets.list("")`:
     *  that enumeration API returns an empty array on some devices/build setups,
     *  which silently loaded zero barge models (every phrase then missed while
     *  "hey axon" still worked — the exact on-device symptom). `assets.list` is
     *  still consulted as a secondary source so a dropped-in `barge*.rpw` is
     *  picked up without a code change, but the app never DEPENDS on it. */
    private fun loadBargeModels(frameSamples: Int): List<WakeDetector> {
        val discovered = runCatching {
            assets.list("")?.filter { it.startsWith("barge") && it.endsWith(".rpw") }
        }.getOrNull().orEmpty()
        val names = (BARGE_MODELS + discovered).distinct()
        val out = ArrayList<WakeDetector>()
        for (file in names) {
            val bytes = runCatching { assets.open(file).readBytes() }.getOrNull() ?: continue
            val det = runCatching {
                WakeDetector(bytes, threshold = BARGE_THRESHOLD, avgThreshold = BARGE_AVG_THRESHOLD, name = file.removeSuffix(".rpw"))
            }.getOrNull() ?: continue
            if (det.samplesPerFrame == frameSamples) out.add(det) else det.close()
        }
        // WARN so it survives HiOS's Log.d suppression: the FIRST thing to check
        // on-device — if this isn't the expected phrases, barge-in fell back to
        // "hey axon" only and no threshold change would help.
        Log.w(LOG_TAG, "barge models loaded: ${out.size} [${out.joinToString { it.name }}]")
        // Visible confirmation without adb: since the device can't be logcat'd,
        // a one-time toast tells the user whether the barge phrases actually
        // loaded (the count should be the full BARGE_MODELS set). TEMP — remove
        // once barge-in is confirmed working on-device.
        Handler(Looper.getMainLooper()).post {
            Toast.makeText(this, "Barge phrases ready: ${out.size}", Toast.LENGTH_LONG).show()
        }
        return out
    }

    @SuppressLint("MissingPermission")
    private fun openRecord(
        source: Int = MediaRecorder.AudioSource.VOICE_RECOGNITION,
    ): AudioRecord? {
        val minBuf = AudioRecord.getMinBufferSize(
            WavRecorder.SAMPLE_RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT
        )
        val rec = try {
            AudioRecord(
                source,
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
        // Intentionally NO AcousticEchoCanceler / NoiseSuppressor on the default
        // VOICE_RECOGNITION mic. That AudioRecord feeds the always-on rustpotter
        // detector, and the VOICE_RECOGNITION source is already the clean signal
        // the wake model was trained on — NS/AEC strip the quiet, far-field wake
        // word right out (regression from fe4e1fe). Follow-up self-echo is
        // handled by the settle + drain + 3-tick gate below, not by mangling the
        // capture. The BARGE mic is the opposite case — loud self-playback the
        // whole time it exists — and opens with VOICE_COMMUNICATION + AEC via
        // [openBargeRecord] instead.
        try {
            rec.startRecording()
        } catch (_: Exception) {
            rec.release()
            return null
        }
        return rec
    }

    /** The echo-cancelled capture for barge-in monitoring, plus the audio
     *  effects that must be released with it. */
    private class BargeRecord(
        val rec: AudioRecord,
        private val fx: List<android.media.audiofx.AudioEffect>,
        /** Whether the AEC effect reports enabled — for the on-screen diag. */
        val aecOn: Boolean,
    ) {
        fun release() {
            rec.release()
            fx.forEach { runCatching { it.release() } }
        }
    }

    /** Open the mic that watches for barge phrases WHILE a reply is playing.
     *
     *  Unlike the idle wake mic, this capture's whole lifetime is spent next to
     *  a speaker blasting the reply: the frames rustpotter sees are the user's
     *  phrase MIXED with the reply's echo, and the mixture matches nobody's
     *  templates — on-device, barge phrases only landed in the silence between
     *  sentences. No threshold fixes a drowned signal; the echo must come OUT of
     *  the capture. So: VOICE_COMMUNICATION source, whose HAL echo canceler
     *  subtracts the phone's own playback (it has the reference signal — this is
     *  exactly its job), leaving the user's voice for the keyword match.
     *
     *  The earlier VOICE_COMMUNICATION attempt was abandoned for its AGC, not
     *  its AEC (clamped capture at full volume, boosted at moderate volume) —
     *  so AGC and NS are explicitly disabled on the session, keeping only the
     *  echo canceler. Where a ROM ignores the disable, the user can fall back
     *  to the old clean VOICE_RECOGNITION capture via the settings toggle
     *  ([Prefs.bargeAecEnabled] off) — no rebuild needed to A/B the two. */
    @SuppressLint("MissingPermission")
    private fun openBargeRecord(): BargeRecord? {
        val rec = openRecord(MediaRecorder.AudioSource.VOICE_COMMUNICATION) ?: return null
        val fx = ArrayList<android.media.audiofx.AudioEffect>()
        var aecOn = false
        runCatching {
            android.media.audiofx.AcousticEchoCanceler.create(rec.audioSessionId)?.let {
                it.enabled = true
                aecOn = it.enabled
                fx.add(it)
            }
        }
        runCatching {
            android.media.audiofx.AutomaticGainControl.create(rec.audioSessionId)?.let {
                it.enabled = false
                fx.add(it)
            }
        }
        runCatching {
            android.media.audiofx.NoiseSuppressor.create(rec.audioSessionId)?.let {
                it.enabled = false
                fx.add(it)
            }
        }
        // WARN so it survives HiOS's Log.d suppression — whether the AEC effect
        // actually attached is the first thing to check when barge still misses.
        Log.w(LOG_TAG, "barge mic: VOICE_COMMUNICATION aec=$aecOn fx=${fx.size}")
        return BargeRecord(rec, fx, aecOn)
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

    private fun interact(rec: AudioRecord, detector: WakeDetector, bargeWakes: List<WakeDetector>) {
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
        // Non-null right after a confirmed barge-in: signals the next iteration
        // to skip the ack and capture the user's post-keyword command instead.
        // Cleared once consumed. (Its bytes — the pre-keyword pre-roll — are NOT
        // used as capture seed: at the barge moment the reply was still playing,
        // so that audio is the reply's own echo.)
        var bargedPreroll: ByteArray? = null
        // What the interrupted reply had spoken so far — used only as the
        // self-echo reference for the barge capture (so the reply bouncing back
        // is rejected instead of looped). Cleared once consumed.
        var bargedSpoken = ""
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
                // pause, would just talk over them again — but do NOT seed the
                // capture with the pre-keyword pre-roll: at the barge moment the
                // reply was still playing, so that audio is the reply's own echo,
                // which was being transcribed back as the user's turn (the
                // self-summarizing / apology loop). Let the aborted reply's echo
                // tail drain out of the speaker + mic buffer, then capture only
                // what the user actually says next (their command follows the
                // "okay"/"wait" keyword), held to the strict fresh-wake onset.
                Thread.sleep(AUDIO_SETTLE_MS)
                drain(rec)
                watcher = SilenceWatcher()
                wav = capture(rec, watcher)
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
                    // follow-up window still raises the level bar, and its length
                    // (how long to wait for the user to start answering) is a
                    // user setting so it doesn't close before they respond.
                    SilenceWatcher(
                        speechRms = SilenceWatcher.FOLLOWUP_RMS,
                        noSpeechTicks = prefs.followupWindowTicks,
                    )
                }
                wav = capture(rec, watcher)
            }
            if (!watcher.hadSpeech) break

            notify(getString(R.string.status_thinking))
            VoiceOverlay.setPhase(VoiceOverlay.Phase.THINKING)
            val text = runCatching { client.transcribe(wav) }.getOrNull()
            if (text.isNullOrBlank()) break
            // A capture that is just our own voice bounced back (ack phrase, the
            // last full reply, or — on a barge — the reply we were mid-way
            // through when interrupted) must not become the next command: with
            // session history the agent would re-answer its own words, looping
            // the reply. This is the safety net for the barge self-echo loop —
            // if the capture is only the interrupted reply's words (no real new
            // command), drop it so the exchange ends instead of feeding the
            // assistant its own voice. A genuine interrupt command ("wait, how
            // about Judges?") is not a subset of the reply, so it passes.
            val echoRef = if (barging) bargedSpoken else lastReply
            if (isSelfEcho(text, echoRef, ackPhrase)) break
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
            val outcome = awaitStreamBlocking(taskForAgent, sessionId, rec, detector, bargeWakes)
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
                bargedSpoken = outcome.spokenSoFar
                // The user deliberately interrupted to say something new. Tell
                // the agent to just answer it — NOT to apologize for the
                // interruption or recap the cut-off reply. That apology-and-recap
                // (compounding with any echo in the capture) was what drove the
                // reply to keep talking over itself.
                pendingNote = "(The user interrupted your previous reply to say " +
                    "something new. Answer their next message directly; do not " +
                    "apologize for the interruption or repeat the interrupted " +
                    "reply unless they ask.) "
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
     * barge-in — the user saying the wake word or a short barge word
     * ("okay"/"wait"), each a rustpotter keyword (see [BargeMonitor]) — and
     * stops the reply when one fires.
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
        bargeWakes: List<WakeDetector>,
    ): BargeOutcome {
        val p = player ?: return BargeOutcome(false)
        val c = chat ?: return BargeOutcome(false)
        var waits = 0
        while (!c.connected && waits++ < 10 && alive) Thread.sleep(500)

        notify(getString(R.string.status_speaking))
        VoiceOverlay.setPhase(VoiceOverlay.Phase.SPEAKING)
        // Whether this reply will actually be barge-monitored — decided up
        // front (before the stream/task even exist) because it drives THREE
        // things that all have to be in place before the first sentence
        // plays: fine-grained TTS chunking, the inter-chunk silence gap, and
        // the echo-cancel route. See the long comment below for why.
        val monitorDetectors = listOfNotNull(wakeDetector) + bargeWakes
        val wantMonitor = rec != null && prefs.bargeInEnabled && monitorDetectors.isNotEmpty()
        val latch = CountDownLatch(1)
        val outcome = java.util.concurrent.atomic.AtomicReference(BargeOutcome(false))
        val stream = StreamingTts(
            player = p,
            client = client,
            cacheDir = cacheDir,
            filePrefix = "reply_wake",
            fineChunking = wantMonitor,
        ) { latch.countDown() }
        synchronized(replyLock) {
            replyLatch = latch
            replyText = null
            replyError = null
            replyStream = stream
        }
        // Monitor for a barge-in while the reply streams — only if the user has
        // barge-in turned on AND there's a live mic AND at least one keyword
        // engine to listen with. Off (or mic-less), and the reply just plays out
        // to completion: the user waits for it to finish before speaking again,
        // which is the whole point of the toggle. Barge-in is keyword spotting
        // (the wake word plus the barge-phrase model), not an energy/echo
        // threshold — so it can't be fooled by the reply's own voice at full
        // volume, and nothing ducks or pumps.
        //
        // MEASURED OFFLINE (mixing real barge-phrase recordings into real reply
        // audio and re-running the shipped detector, since no adb-reachable
        // device exists for this project): keyword recall is 18/18 in genuine
        // silence and collapses to a near-permanent floor (~6/18) the instant
        // ANY reply audio overlaps the phrase, even at a small fraction of the
        // phrase recording's own level — this is not a threshold/SNR problem,
        // rustpotter's matching just doesn't tolerate full-duplex audio at all.
        // So the ONLY thing that reliably restores recall is a genuinely silent
        // window — which is exactly why the user could only ever land a barge-in
        // by timing a sentence pause: that pause was the one moment the mic
        // heard silence, and it was luck-of-the-draw because it was just
        // whatever gap synth network jitter happened to leave, not a deliberate
        // one. `stream.fineChunking` (set above) turns "one gap per sentence"
        // into "one gap per clause" by also splitting TTS synthesis at clause
        // commas; `p.interChunkGapMs` below makes each of those gaps an actual
        // guaranteed silence instead of an accidental leftover one.
        //
        // [enterCommRoute] (VOICE_COMMUNICATION playback + AEC capture) is kept
        // as a secondary layer that might claw back a little more recall DURING
        // active speech on devices whose AEC genuinely engages — but the
        // measurement above shows it can't be the whole fix: even a fairly good
        // real-world echo canceller's residual is nowhere near quiet enough to
        // clear the cliff this detector has.
        val wantAec = wantMonitor && prefs.bargeAecEnabled
        var prevAudioMode: Int? = null
        if (wantAec) prevAudioMode = enterCommRoute(p)
        if (wantMonitor) p.interChunkGapMs = BARGE_INTER_CHUNK_GAP_MS
        if (!c.sendTask(task, sessionId, voice = true)) {
            stream.abort()
            synchronized(replyLock) { replyStream = null }
            prevAudioMode?.let { exitCommRoute(p, it) }
            p.interChunkGapMs = 0
            return BargeOutcome(false)
        }
        // Open a FRESH capture for the reply's duration rather than reusing the
        // pre-existing wake mic. A fresh AudioRecord binds to the audio route as
        // it is once assistant playback has started; the pre-existing wake mic
        // (opened before playback) goes silent on some ROMs when the output
        // route takes over — which made barge-in detect nothing at all during a
        // reply. The fresh mic is ECHO-CANCELLED by default (VOICE_COMMUNICATION
        // + AEC, see [openBargeRecord]) to pair with the comm-routed playback
        // above. [Prefs.bargeAecEnabled] off falls back to the old clean
        // VOICE_RECOGNITION capture on the normal playback route. Only one mic
        // captures at a time, so the wake mic is stopped first and restarted
        // afterwards for the follow-up capture / idle listening. If the fresh
        // mic won't open, fall back to the wake mic.
        var bargeMic: AudioRecord? = null
        var bargeRec: BargeRecord? = null
        var micTag = "wake" // which capture the monitor ended up on — for diag
        val monitorRec: AudioRecord? =
            if (wantMonitor) {
                runCatching { rec!!.stop() }
                if (wantAec) {
                    bargeRec = openBargeRecord()
                    micTag = if (bargeRec?.aecOn == true) "ec" else "vc"
                } else {
                    bargeMic = openRecord()
                    micTag = "vr"
                }
                bargeRec?.rec ?: bargeMic ?: run {
                    runCatching { rec!!.startRecording() } // fresh mic unavailable — reuse the wake mic
                    micTag = "wake"
                    rec
                }
            } else null
        val monitor = if (monitorRec != null) {
            thread(name = "axon-barge") {
                BargeMonitor(
                    wakeDetectors = monitorDetectors,
                    readFrame = { f -> fillFrame(monitorRec, f) },
                    onConfirmed = { preroll ->
                        Log.d(LOG_TAG, "barge CONFIRMED (keyword)")
                        val spoken = stream.spokenSoFar()
                        stream.abort() // cut the TTS mid-sentence
                        c.cancel(sessionId) // stop generating, not just talking
                        outcome.set(BargeOutcome(true, preroll, spoken))
                        latch.countDown()
                    },
                    // TEMP: surface the live mic level + best keyword score in the
                    // notification so a device with no adb can still read them.
                    // Say a phrase during a reply and watch these numbers. The tag
                    // names the capture in use: ec = echo-cancelled (AEC active),
                    // vc = VOICE_COMMUNICATION but AEC effect didn't attach,
                    // vr = clean VOICE_RECOGNITION (toggle off), wake = fallback.
                    onDiag = { rms, maxScore ->
                        notify("barge[%s]: mic=%.3f score=%.2f".format(micTag, rms, maxScore))
                    },
                ).run { !alive || latch.count == 0L }
            }
        } else null
        latch.await(310, TimeUnit.SECONDS)
        monitor?.join(500)
        // Undo the voice-comm routing FIRST so the follow-up capture and idle
        // wake listening rebind on the normal route, then release the fresh
        // barge mic (and its AEC session effects) and hand the microphone back
        // to the always-on wake mic.
        prevAudioMode?.let { exitCommRoute(p, it) }
        p.interChunkGapMs = 0
        if (bargeRec != null || bargeMic != null) {
            bargeRec?.release()
            bargeMic?.release()
            runCatching { rec?.startRecording() }
        }
        val result = outcome.get()
        if (result.barged) drain(rec!!)
        return result
    }

    /** Move this reply's playback onto the voice-communication path —
     *  MODE_IN_COMMUNICATION, speakerphone, and [TtsPlayer.commAudio] — so the
     *  platform echo canceler on the barge mic actually references the reply
     *  as its far-end signal and cancels it out of the capture. Returns the
     *  previous audio mode for [exitCommRoute], or null if routing failed
     *  (playback then stays on the normal path; barge-in degrades to the old
     *  pause-only behavior rather than breaking playback).
     *
     *  Trade-off: while routed, playback level follows the CALL volume, not
     *  media volume. */
    private fun enterCommRoute(p: TtsPlayer): Int? = runCatching {
        val am = getSystemService(Context.AUDIO_SERVICE) as android.media.AudioManager
        val prev = am.mode
        am.mode = android.media.AudioManager.MODE_IN_COMMUNICATION
        if (Build.VERSION.SDK_INT >= 31) {
            am.availableCommunicationDevices
                .firstOrNull { it.type == android.media.AudioDeviceInfo.TYPE_BUILTIN_SPEAKER }
                ?.let { am.setCommunicationDevice(it) }
        } else {
            @Suppress("DEPRECATION")
            am.isSpeakerphoneOn = true
        }
        p.commAudio = true
        Log.w(LOG_TAG, "barge comm route ON (prev mode=$prev)")
        prev
    }.getOrNull()

    /** Undo [enterCommRoute]: normal playback attrs, communication device
     *  cleared, audio mode restored. */
    private fun exitCommRoute(p: TtsPlayer, prevMode: Int) {
        p.commAudio = false
        runCatching {
            val am = getSystemService(Context.AUDIO_SERVICE) as android.media.AudioManager
            if (Build.VERSION.SDK_INT >= 31) {
                am.clearCommunicationDevice()
            } else {
                @Suppress("DEPRECATION")
                am.isSpeakerphoneOn = false
            }
            am.mode = prevMode
        }
        Log.w(LOG_TAG, "barge comm route OFF")
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
