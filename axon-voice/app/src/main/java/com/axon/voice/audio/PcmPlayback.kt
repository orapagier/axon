package com.axon.voice.audio

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.media.MediaCodec
import android.media.MediaExtractor
import android.media.MediaFormat
import java.io.File
import java.io.RandomAccessFile
import java.util.ArrayDeque
import kotlin.math.min
import kotlin.math.sqrt

/**
 * Plays one synthesized-speech file and, while it plays, emits the live 0..1 RMS
 * of the audio **currently at the DAC** — the SPEAKING-phase counterpart to
 * [WavRecorder]'s LISTENING RMS. This is what lets the voice orb react to the
 * real reply in realtime instead of a synthetic envelope. The level is on the
 * same scale [WavRecorder] feeds the orb for the mic, so both phases speak the
 * same units.
 *
 * Why not [android.media.MediaPlayer] + a [android.media.audiofx.Visualizer]
 * tap? Visualizer is a privacy-gated system effect that silently returns
 * silence on many OEM ROMs and every emulator, and gives no clock to sync to.
 * Decoding to PCM ourselves is deterministic on every device and lets us align
 * the level to [AudioTrack.getPlaybackHeadPosition] — so the orb tracks what is
 * *audible*, lagging by at most one ~20ms window rather than leading the sound.
 *
 * The server's /audio/speech is a passthrough, so the bytes are WAV (Gemini,
 * Piper) or MP3/other (Groq, OpenAI). [MediaExtractor] does not reliably open
 * WAV, so WAV is parsed directly and everything else goes through [MediaCodec].
 *
 * One instance plays one file. [start] runs decode+playback on its own daemon
 * thread; [onEnd] fires exactly once on natural completion or a decode failure,
 * and never after [stop] (a barge-in / takeover). Mirrors the old MediaPlayer
 * completion/error → onDone contract [TtsPlayer] relies on.
 */
internal class PcmPlayback(
    private val file: File,
    private val attrs: AudioAttributes,
    /** 0..1 RMS at the DAC, or -1 when this file stops playing. Called off the
     *  playback thread — keep it cheap. */
    private val onLevel: ((Float) -> Unit)?,
    /** Natural completion or decode failure — not cancellation. Fires once. */
    private val onEnd: () -> Unit,
) {
    @Volatile
    private var cancelled = false

    /** The live track, exposed only so [stop] can silence it from another
     *  thread; the playback thread owns its full lifecycle otherwise. */
    @Volatile
    private var track: AudioTrack? = null

    /** Output volume (0..1) applied to [track] as soon as it opens, and live
     *  via [setVolume] before then — the barge-in duck/restore hook. */
    @Volatile
    private var volume = 1f

    private val worker = Thread({ run() }, "axon-tts-pcm").apply { isDaemon = true }

    fun start() = worker.start()

    val playing: Boolean get() = worker.isAlive && !cancelled

    /** Cancel playback now; [onEnd] will not fire. Safe from any thread. A
     *  blocking [AudioTrack.write] returns early once the track is paused, so
     *  the worker sees [cancelled] within a window and tears the track down. */
    fun stop() {
        cancelled = true
        runCatching { track?.pause() }
    }

    /** Set the live output volume (0..1); applies to [track] immediately if
     *  one is already open, and to any track this instance opens after. Safe
     *  from any thread — the barge-in monitor calls this off its own loop. */
    fun setVolume(v: Float) {
        volume = v
        track?.let { runCatching { it.setVolume(v) } }
    }

    private fun run() {
        runCatching { decodeAndPlay() }
        if (!cancelled) {
            onLevel?.invoke(-1f) // hand the orb back to its synthetic envelope
            onEnd()
        }
    }

    private fun decodeAndPlay() {
        val magic = ByteArray(12)
        val read = file.inputStream().use { input ->
            var r = 0
            while (r < 12) {
                val n = input.read(magic, r, 12 - r)
                if (n < 0) break
                r += n
            }
            r
        }
        val wav = read == 12 &&
            String(magic, 0, 4) == "RIFF" && String(magic, 8, 4) == "WAVE"
        if (wav) playWav() else playEncoded()
    }

    // ── WAV (raw PCM, no codec) ──────────────────────────────────────────────

    private fun playWav() {
        RandomAccessFile(file, "r").use { raf ->
            var sampleRate = 0
            var channels = 0
            var bits = 0
            var dataPos = -1L
            var dataLen = 0L
            raf.seek(12) // past "RIFF" <size> "WAVE"
            val header = ByteArray(8)
            while (dataPos < 0 && readFully(raf, header)) {
                val id = String(header, 0, 4)
                val size = leInt(header, 4).toLong() and 0xffffffffL
                when (id) {
                    "fmt " -> {
                        val fmt = ByteArray(size.toInt())
                        if (!readFully(raf, fmt)) return
                        channels = leShort(fmt, 2)
                        sampleRate = leInt(fmt, 4)
                        bits = leShort(fmt, 14)
                        if (size and 1L == 1L) raf.seek(raf.filePointer + 1) // pad byte
                    }
                    "data" -> {
                        dataPos = raf.filePointer
                        dataLen = size
                    }
                    else -> raf.seek(raf.filePointer + size + (size and 1L))
                }
            }
            if (dataPos < 0 || bits != 16 || sampleRate <= 0 || channels <= 0) return
            raf.seek(dataPos)
            val sink = Sink(sampleRate, channels)
            try {
                val buf = ByteArray(sink.windowBytes)
                var remaining = dataLen
                while (remaining > 0 && !cancelled) {
                    val want = min(remaining, buf.size.toLong()).toInt()
                    val n = raf.read(buf, 0, want)
                    if (n <= 0) break
                    sink.feed(buf, n)
                    remaining -= n
                }
            } finally {
                sink.finish()
            }
        }
    }

    // ── Everything else (MP3/AAC/OGG…) via the platform decoder ──────────────

    private fun playEncoded() {
        val extractor = MediaExtractor()
        var codec: MediaCodec? = null
        var sink: Sink? = null
        try {
            extractor.setDataSource(file.absolutePath)
            var index = -1
            var format: MediaFormat? = null
            for (i in 0 until extractor.trackCount) {
                val f = extractor.getTrackFormat(i)
                if (f.getString(MediaFormat.KEY_MIME)?.startsWith("audio/") == true) {
                    index = i
                    format = f
                    break
                }
            }
            if (index < 0 || format == null) return
            extractor.selectTrack(index)
            val mime = format.getString(MediaFormat.KEY_MIME) ?: return
            val decoder = MediaCodec.createDecoderByType(mime)
            codec = decoder
            decoder.configure(format, null, null, 0)
            decoder.start()
            val info = MediaCodec.BufferInfo()
            var inputDone = false
            var outputDone = false
            while (!outputDone && !cancelled) {
                if (!inputDone) {
                    val inIndex = decoder.dequeueInputBuffer(10_000)
                    if (inIndex >= 0) {
                        val inBuf = decoder.getInputBuffer(inIndex)
                        val n = if (inBuf != null) extractor.readSampleData(inBuf, 0) else -1
                        if (n < 0) {
                            decoder.queueInputBuffer(
                                inIndex, 0, 0, 0, MediaCodec.BUFFER_FLAG_END_OF_STREAM,
                            )
                            inputDone = true
                        } else {
                            decoder.queueInputBuffer(inIndex, 0, n, extractor.sampleTime, 0)
                            extractor.advance()
                        }
                    }
                }
                val outIndex = decoder.dequeueOutputBuffer(info, 10_000)
                if (outIndex >= 0) {
                    if (info.flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0) outputDone = true
                    val config = info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0
                    if (info.size > 0 && !config) {
                        val outBuf = decoder.getOutputBuffer(outIndex)
                        if (outBuf != null) {
                            val s = sink ?: Sink(
                                decoder.outputFormat.getInteger(MediaFormat.KEY_SAMPLE_RATE),
                                decoder.outputFormat.getInteger(MediaFormat.KEY_CHANNEL_COUNT),
                            ).also { sink = it }
                            val chunk = ByteArray(info.size)
                            outBuf.position(info.offset)
                            outBuf.limit(info.offset + info.size)
                            outBuf.get(chunk)
                            s.feed(chunk, chunk.size)
                        }
                    }
                    decoder.releaseOutputBuffer(outIndex, false)
                }
            }
        } finally {
            runCatching { codec?.stop() }
            runCatching { codec?.release() }
            runCatching { extractor.release() }
            sink?.finish()
        }
    }

    // ── PCM sink: writes to AudioTrack and emits the DAC-aligned level ────────

    /**
     * Owns one [AudioTrack] and streams PCM16 into it in ~20ms windows. For each
     * window it records the RMS tagged with the sample index where it starts,
     * then emits the RMS of whichever window the playback head has just reached
     * — so [onLevel] reflects audible sound, not what was merely queued.
     */
    private inner class Sink(sampleRate: Int, channels: Int) {
        private val bytesPerFrame = channels * 2
        val windowBytes = (sampleRate / 50).coerceAtLeast(64) * bytesPerFrame // ~20ms
        private val out = openTrack(sampleRate, channels)
        private var framesWritten = 0L
        private val marks = ArrayDeque<Mark>()

        init {
            track = out
            out.play()
        }

        /** Write [len] bytes of PCM16 from [pcm], emitting the level as it goes. */
        fun feed(pcm: ByteArray, len: Int) {
            var off = 0
            while (off < len && !cancelled) {
                val n = min(windowBytes, len - off)
                marks.addLast(Mark(framesWritten, rms(pcm, off, n)))
                val end = off + n
                var w = off
                while (w < end && !cancelled) {
                    // Non-blocking so a barge-in (stop -> pause + cancelled) can
                    // never wedge the worker waiting on a full buffer. When the
                    // buffer is full, let the DAC drain a little — which also
                    // paces decoding to realtime. A negative return is a track
                    // error: bail so onEnd fires (never-silent fallback), like
                    // the old MediaPlayer onError path.
                    val wrote = out.write(pcm, w, end - w, AudioTrack.WRITE_NON_BLOCKING)
                    if (wrote < 0) throw IllegalStateException("AudioTrack.write $wrote")
                    if (wrote == 0) {
                        emit()
                        Thread.sleep(5)
                        continue
                    }
                    w += wrote
                }
                framesWritten += n / bytesPerFrame
                emit()
                off = end
            }
        }

        private fun emit() {
            val head = out.playbackHeadPosition.toLong() and 0xffffffffL
            var level = -1f
            while (true) {
                val mark = marks.peekFirst() ?: break
                if (mark.frame > head) break
                marks.pollFirst()
                level = mark.rms
            }
            if (level >= 0f) onLevel?.invoke(level)
        }

        /** Drain the buffered tail (still emitting) then release the track. The
         *  cap bounds both a normal tail (~one buffer) and a stalled/errored
         *  track, so this never hangs the worker. */
        fun finish() {
            if (!cancelled) runCatching { out.stop() } // play out what's buffered
            val deadline = System.nanoTime() + 500_000_000L
            while (!cancelled && marks.isNotEmpty() && System.nanoTime() < deadline) {
                emit()
                Thread.sleep(15)
            }
            runCatching { out.flush() }
            runCatching { out.release() }
            if (track === out) track = null
        }
    }

    private fun openTrack(sampleRate: Int, channels: Int): AudioTrack {
        val mask = if (channels >= 2) AudioFormat.CHANNEL_OUT_STEREO else AudioFormat.CHANNEL_OUT_MONO
        val format = AudioFormat.Builder()
            .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
            .setSampleRate(sampleRate)
            .setChannelMask(mask)
            .build()
        val minBuf = AudioTrack.getMinBufferSize(sampleRate, mask, AudioFormat.ENCODING_PCM_16BIT)
        val floor = sampleRate / 10 * channels * 2 // ~100ms — bounds start/stop latency
        return AudioTrack.Builder()
            .setAudioAttributes(attrs)
            .setAudioFormat(format)
            .setBufferSizeInBytes(maxOf(minBuf, floor))
            .setTransferMode(AudioTrack.MODE_STREAM)
            .build()
            .also { runCatching { it.setVolume(volume) } }
    }

    private class Mark(val frame: Long, val rms: Float)

    /** RMS of a PCM16 little-endian span, 0..1 — matches [WavRecorder]'s scale
     *  (sample / 32768). Channels are irrelevant to a level, so all samples are
     *  pooled. */
    private fun rms(b: ByteArray, off: Int, len: Int): Float {
        var acc = 0.0
        var count = 0
        var i = off
        val end = off + (len - len % 2)
        while (i < end) {
            val s = (b[i].toInt() and 0xff) or (b[i + 1].toInt() shl 8)
            val f = s / 32768.0
            acc += f * f
            count++
            i += 2
        }
        return if (count == 0) 0f else sqrt(acc / count).toFloat()
    }

    private fun readFully(raf: RandomAccessFile, buf: ByteArray): Boolean {
        var r = 0
        while (r < buf.size) {
            val n = raf.read(buf, r, buf.size - r)
            if (n < 0) return false
            r += n
        }
        return true
    }

    private fun leShort(b: ByteArray, o: Int) =
        (b[o].toInt() and 0xff) or ((b[o + 1].toInt() and 0xff) shl 8)

    private fun leInt(b: ByteArray, o: Int) =
        (b[o].toInt() and 0xff) or ((b[o + 1].toInt() and 0xff) shl 8) or
            ((b[o + 2].toInt() and 0xff) shl 16) or ((b[o + 3].toInt() and 0xff) shl 24)
}
