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
import com.axon.voice.Prefs
import com.axon.voice.R
import com.axon.voice.api.AxonClient
import com.axon.voice.api.ChatSocket
import com.axon.voice.audio.Sound
import com.axon.voice.audio.SilenceWatcher
import com.axon.voice.audio.StreamingTts
import com.axon.voice.audio.TtsPlayer
import com.axon.voice.audio.VoicePrompts
import com.axon.voice.audio.WavRecorder
import com.axon.voice.ui.ChatFeed
import com.axon.voice.ui.ChatActivity
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
        private const val CHANNEL = "axon_voice"
        private const val NOTIF_ID = 1
        const val ACTION_STOP = "com.axon.voice.STOP_WAKE"

        /** MediaPlayer's completion callback can fire a few hundred ms before
         *  the audio sink actually finishes playing out (worse on Bluetooth).
         *  Waiting this long before the pre-capture drain keeps our own ack /
         *  reply tail out of the follow-up capture. */
        private const val AUDIO_SETTLE_MS = 250L

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

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        prefs = Prefs(this)
        client = AxonClient(prefs)
        player = TtsPlayer(this)
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
                    interact(rec, detector)
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
        var first = true
        var lastReply = ""
        while (alive && !micHold) {
            notify(getString(R.string.status_recording))
            // A wake is answered out loud; the follow-up window opens on its
            // soft chime alone — no spoken prompt. Sound.chime is asynchronous,
            // so hold here for the note (plus the settle below) and let the
            // drain clear it rather than have its tail open the capture.
            val ackPhrase = if (first) VoicePrompts.randomWakeAck() else ""
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
                SilenceWatcher(
                    speechRms = SilenceWatcher.FOLLOWUP_RMS,
                    minSpeechTicks = SilenceWatcher.FOLLOWUP_MIN_SPEECH_TICKS,
                )
            }
            val wav = capture(rec, watcher)
            if (!watcher.hadSpeech) break

            notify(getString(R.string.status_thinking))
            val text = runCatching { client.transcribe(wav) }.getOrNull()
            if (text.isNullOrBlank()) break
            // A capture that is just our own voice bounced back (ack phrase or
            // a fragment of the reply we just spoke) must not become the next
            // command — with session history the agent would happily re-answer
            // the previous question in new words, looping the reply.
            if (isSelfEcho(text, lastReply, ackPhrase)) break
            // The accepted command is part of the chat conversation from here:
            // it shows in the Chat page (live, if open) like a typed message.
            ChatFeed.post(this, prefs.chatSessionId, "user", text)
            // Stream the reply: tokens flow into StreamingTts as they arrive,
            // so the first sentence plays ~1s after the agent starts replying
            // instead of after the whole reply is synthesized. Blocks until
            // playback finishes or a barge-in cuts it off.
            val barged = awaitStreamBlocking(text, rec, detector)
            val (reply, err) = synchronized(replyLock) { (replyText ?: "") to replyError }
            if (reply.isNotBlank()) {
                ChatFeed.post(this, prefs.chatSessionId, "assistant", reply)
            } else if (!barged && err != null) {
                ChatFeed.post(this, prefs.chatSessionId, "error", "Sorry — $err")
            }
            if (reply.isBlank() && !barged) break
            if (barged) {
                // User said "Hey Axon" mid-reply. Ack and treat the next
                // capture as a fresh wake — lenient watcher, no self-echo
                // check against the half-spoken reply we just cut off.
                first = true
                lastReply = ""
                continue
            }
            lastReply = reply
            first = false // reopen as the follow-up window, raised speech bar
        }
        notify(getString(R.string.notif_listening))
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

    private fun capture(rec: AudioRecord, watcher: SilenceWatcher): ByteArray {
        val out = ByteArrayOutputStream()
        val buf = ShortArray(WavRecorder.SAMPLE_RATE / 10)
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
            if (watcher.tick(sqrt(acc / n))) break
        }
        return WavRecorder.wavBytes(out.toByteArray())
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

    /**
     * Send [task] and stream the reply: tokens are fed to a [StreamingTts] as
     * they arrive so per-sentence TTS begins with the first sentence, not after
     * the whole reply is downloaded. Watches the mic for a "Hey Axon" barge-in
     * the whole time. Returns true if the user interrupted; false if playback
     * finished on its own.
     *
     * The barge-in detector runs on a throwaway thread that keeps feeding the
     * same rustpotter [detector] used by runLoop. It's safe because runLoop is
     * parked inside interact()→awaitStreamBlocking() for the entire playback,
     * so process(frame) is never called concurrently from two threads.
     */
    private fun awaitStreamBlocking(
        task: String,
        rec: AudioRecord?,
        detector: WakeDetector?,
    ): Boolean {
        val p = player ?: return false
        val c = chat ?: return false
        var waits = 0
        while (!c.connected && waits++ < 10 && alive) Thread.sleep(500)

        notify(getString(R.string.status_speaking))
        val latch = CountDownLatch(1)
        val bargedIn = java.util.concurrent.atomic.AtomicBoolean(false)
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
        if (!c.sendTask(task, prefs.chatSessionId)) {
            stream.abort()
            synchronized(replyLock) { replyStream = null }
            return false
        }
        // Monitor for "Hey Axon" while the reply streams. Only when we have
        // both a live mic and the detector — otherwise we just wait for done.
        val monitor = if (rec != null && detector != null) {
            thread(name = "axon-barge") {
                val frame = ShortArray(detector.samplesPerFrame)
                while (alive && latch.count > 0L) {
                    if (!fillFrame(rec, frame)) break
                    if (detector.process(frame) >= 0f) {
                        bargedIn.set(true)
                        stream.abort() // cut the TTS mid-sentence
                        latch.countDown()
                        break
                    }
                }
            }
        } else null
        latch.await(310, TimeUnit.SECONDS)
        monitor?.join(500)
        if (bargedIn.get()) drain(rec!!)
        return bargedIn.get()
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
