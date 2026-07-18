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
import com.axon.voice.audio.MicEffects
import com.axon.voice.audio.Sound
import com.axon.voice.audio.SilenceWatcher
import com.axon.voice.audio.TtsPlayer
import com.axon.voice.audio.WavRecorder
import com.axon.voice.ui.MainActivity
import org.json.JSONObject
import java.io.ByteArrayOutputStream
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread
import kotlin.math.sqrt

/**
 * Always-on "Hey Axon" listener: a microphone foreground service running the
 * rustpotter detector on a continuous 16k capture. On detection it speaks the
 * "Yes?" ack, captures the command with the ported silence watcher, ships it
 * through transcribe -> /ws task -> TTS, then opens the follow-up window
 * ("Anything else?") with the raised bystander bar — the same flow as the
 * dashboard wake word, minus the browser.
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

    private var ackYes: File? = null
    private var ackMore: File? = null
    private var effects: MicEffects? = null

    // One reply in flight at a time: sendAndAwait parks on the latch, the WS
    // listener below fills the slot.
    private val replyLock = Object()
    private var replyLatch: CountDownLatch? = null
    private var replyText: String? = null
    private var replyError: String? = null

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
        prefetchAcks()

        var record: AudioRecord? = null
        try {
            val frame = ShortArray(detector.samplesPerFrame)
            while (alive) {
                if (micHold) {
                    record?.release()
                    record = null
                    effects?.release()
                    effects = null
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
            effects?.release()
            effects = null
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
        effects?.release()
        effects = MicEffects(rec.audioSessionId)
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

    private fun interact(rec: AudioRecord) {
        var first = true
        var lastReply = ""
        while (alive && !micHold) {
            notify(getString(R.string.status_recording))
            playAckBlocking(if (first) ackYes else ackMore, soft = !first)
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
            if (isSelfEcho(text, lastReply, if (first) "yes" else "anything else")) break
            val reply = sendAndAwait(text) ?: break
            if (reply.isNotBlank()) {
                notify(getString(R.string.status_speaking))
                speakBlocking(reply)
                lastReply = reply
            }
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

    /** "Yes?" / "Anything else?" via prefetched server TTS, chime fallback. */
    private fun playAckBlocking(f: File?, soft: Boolean) {
        val p = player ?: return
        if (f != null && f.length() > 0) {
            val latch = CountDownLatch(1)
            p.play(f) { latch.countDown() }
            latch.await(4, TimeUnit.SECONDS)
        } else {
            Sound.chime(soft)
            Thread.sleep(if (soft) 250 else 400)
        }
    }

    private fun speakBlocking(text: String) {
        val p = player ?: return
        val f = File(cacheDir, "reply_wake.audio")
        val ok = client.speech(text, f)
        val latch = CountDownLatch(1)
        if (ok) {
            p.play(f) { latch.countDown() }
        } else {
            p.speakFallback(text) { latch.countDown() }
        }
        latch.await(120, TimeUnit.SECONDS)
    }

    private fun prefetchAcks() {
        ackYes = File(cacheDir, "ack_yes.audio").also {
            if (!it.exists() || it.length() == 0L) {
                if (!client.speech("Yes?", it)) it.delete()
            }
        }
        ackMore = File(cacheDir, "ack_more.audio").also {
            if (!it.exists() || it.length() == 0L) {
                if (!client.speech("Anything else?", it)) it.delete()
            }
        }
    }

    // ── Chat plumbing ───────────────────────────────────────────────────────

    private fun sendAndAwait(task: String): String? {
        val c = chat ?: return null
        var waits = 0
        while (!c.connected && waits++ < 10 && alive) Thread.sleep(500)
        val latch = CountDownLatch(1)
        synchronized(replyLock) {
            replyLatch = latch
            replyText = null
            replyError = null
        }
        if (!c.sendTask(task, prefs.sessionId)) return null
        if (!latch.await(310, TimeUnit.SECONDS)) return null
        synchronized(replyLock) {
            replyError?.let { return "Sorry — $it" }
            return replyText
        }
    }

    override fun onWsEvent(ev: JSONObject) {
        when (ev.optString("type")) {
            "done" -> synchronized(replyLock) {
                replyText = ev.optString("full_text", "")
                replyLatch?.countDown()
                replyLatch = null
            }
            "error" -> synchronized(replyLock) {
                replyError = ev.optString("message", "something went wrong")
                replyLatch?.countDown()
                replyLatch = null
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
            Intent(this, MainActivity::class.java),
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
