package com.axon.voice.ui

import android.content.Context
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.DashPathEffect
import android.graphics.Matrix
import android.graphics.Paint
import android.graphics.RadialGradient
import android.graphics.Shader
import android.util.AttributeSet
import android.view.View
import androidx.core.content.ContextCompat
import com.axon.voice.R
import kotlin.math.abs
import kotlin.math.min
import kotlin.math.sin

/**
 * JARVIS-style reactive orb for the hands-free ("Hey Axon") overlay in
 * [ChatActivity] — the Android counterpart of the web dashboard's VoiceOrb.vue,
 * and a direct port of its draw: a rotating ambient ring, three level-reactive
 * concentric rings, and a radial-gradient core. Runs on the view's own
 * animation timebase (postOnAnimation) rather than any data binding, since it
 * needs a fresh frame every vsync regardless of whether state changed.
 *
 * [setPhase] drives what it shows. LISTENING reacts to the real mic level and
 * SPEAKING to the real reply-audio level, both fed by [setLevel] on the same
 * 0..1 RMS scale (the wake service's capture RMS, and [PcmPlayback]'s
 * DAC-aligned playback RMS respectively). THINKING has no metered signal, and
 * so do the brief gaps between spoken sentences — a synthetic envelope keeps
 * the orb alive there.
 */
class VoiceOrbView @JvmOverloads constructor(
    context: Context,
    attrs: AttributeSet? = null,
    defStyle: Int = 0,
) : View(context, attrs, defStyle) {

    enum class Phase { IDLE, LISTENING, THINKING, SPEAKING }

    companion object {
        // Metered RMS is small; these lift it into a lively 0..1 orb range. Mic
        // capture is quiet (speech ~0.02–0.08) so it needs more gain than the
        // cleaner, louder reply audio. Both empirical — tune on a real device.
        private const val MIC_GAIN = 9f
        private const val SPEAK_GAIN = 4f
    }

    private val accent = ContextCompat.getColor(context, R.color.accent)
    private val glow = ContextCompat.getColor(context, R.color.orb_glow)
    private val bg = ContextCompat.getColor(context, R.color.bg)

    private val ringPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply { style = Paint.Style.STROKE }
    private val corePaint = Paint(Paint.ANTI_ALIAS_FLAG).apply { style = Paint.Style.FILL }
    private val bgPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply { style = Paint.Style.FILL }
    private val dash = DashPathEffect(floatArrayOf(6f, 10f), 0f)

    // Geometry + core gradient are size-derived, so they're built once per
    // layout in onSizeChanged and reused every frame — a fresh RadialGradient
    // (or int/float array) per onDraw would allocate ~60x/sec. The gradient is
    // scaled to the pulsing core each frame via a preallocated matrix instead.
    private var cx = 0f
    private var cy = 0f
    private var baseR = 0f
    private var coreShader: RadialGradient? = null
    private val coreMatrix = Matrix()

    private var phase = Phase.IDLE

    @Volatile
    private var level = -1f // latest mic level, -1 = none (synthesize)

    private var smoothed = 0f
    private var startNanos = 0L
    private var animating = false

    private val frame = Runnable { step() }

    private fun step() {
        if (!animating) return
        invalidate()
        postOnAnimation(frame)
    }

    fun setPhase(p: Phase) {
        if (p == phase) return
        phase = p
        level = -1f
        if (p == Phase.IDLE) stop() else start()
    }

    fun setLevel(l: Float) {
        level = l
    }

    private fun start() {
        if (animating) return
        animating = true
        if (startNanos == 0L) startNanos = System.nanoTime()
        postOnAnimation(frame)
    }

    private fun stop() {
        animating = false
        removeCallbacks(frame)
        invalidate() // clear the last frame
    }

    override fun onSizeChanged(w: Int, h: Int, oldw: Int, oldh: Int) {
        super.onSizeChanged(w, h, oldw, oldh)
        cx = w / 2f
        cy = h / 2f
        baseR = min(w, h) * 0.16f
        // Reference gradient sized to the resting core (baseR * 1.8); onDraw
        // scales it to the live coreR each frame. Built here, not per frame.
        coreShader = RadialGradient(
            cx, cy, baseR * 1.8f,
            intArrayOf(glow, accent, Color.TRANSPARENT),
            floatArrayOf(0f, 0.55f, 1f),
            Shader.TileMode.CLAMP,
        )
    }

    override fun onAttachedToWindow() {
        super.onAttachedToWindow()
        if (phase != Phase.IDLE) start()
    }

    override fun onDetachedFromWindow() {
        super.onDetachedFromWindow()
        stop()
    }

    // Layered sines beat into a syllable-shaped envelope (SPEAKING) or a slow
    // ambient breath (THINKING), so the orb never freezes when nothing is
    // being metered. Range roughly 0..0.44.
    private fun synth(t: Float): Float = when (phase) {
        Phase.SPEAKING -> 0.22f + 0.22f * abs(sin(t * 5.3f)) * (0.55f + 0.45f * sin(t * 1.4f))
        Phase.THINKING -> 0.12f + 0.05f * sin(t * 1.6f)
        else -> 0.08f
    }

    override fun onDraw(canvas: Canvas) {
        if (phase == Phase.IDLE || baseR == 0f) return
        val t = (System.nanoTime() - startNanos) / 1_000_000_000f
        // Real metered level for LISTENING (mic) and SPEAKING (reply audio);
        // synth for THINKING and any un-metered gap between spoken sentences.
        val raw = when {
            phase == Phase.LISTENING && level >= 0f -> min(1f, level * MIC_GAIN)
            phase == Phase.SPEAKING && level >= 0f -> min(1f, level * SPEAK_GAIN)
            else -> synth(t)
        }
        // Snap up fast (speech onset feels immediate), decay slower (a gap
        // between words shouldn't collapse the orb to nothing).
        smoothed += (raw - smoothed) * (if (raw > smoothed) 0.5f else 0.12f)

        val scale = 1f + smoothed * 0.9f // coreR / baseR
        val coreR = baseR * scale

        // Ambient rotating dashed ring — reads as "processing" even at rest.
        canvas.save()
        val spinRad = t * (if (phase == Phase.THINKING) 0.6f else 0.25f)
        canvas.rotate(Math.toDegrees(spinRad.toDouble()).toFloat(), cx, cy)
        ringPaint.pathEffect = dash
        ringPaint.strokeWidth = 2f
        ringPaint.color = glow
        ringPaint.alpha = 64
        canvas.drawCircle(cx, cy, baseR * 2.3f, ringPaint)
        canvas.restore()

        // Reactive concentric rings, staggered outward.
        ringPaint.pathEffect = null
        ringPaint.strokeWidth = 3f
        for (i in 0..2) {
            val spread = baseR * (1.3f + i * 0.45f) + smoothed * baseR * (1.6f + i * 0.5f)
            ringPaint.color = if (i == 0) accent else glow
            ringPaint.alpha = (maxOf(0f, 0.35f - i * 0.1f) * (0.4f + smoothed) * 255f).toInt().coerceIn(0, 255)
            canvas.drawCircle(cx, cy, spread, ringPaint)
        }

        // Core glow — reuse the prebuilt gradient, scaled about the centre to
        // the live core radius via a preallocated matrix (no per-frame alloc).
        coreShader?.let { shader ->
            coreMatrix.setScale(scale, scale, cx, cy)
            shader.setLocalMatrix(coreMatrix)
            corePaint.shader = shader
            corePaint.alpha = 255
            canvas.drawCircle(cx, cy, coreR * 1.8f, corePaint)
            corePaint.shader = null
        }

        // Inner disc punched back to the background so the core reads as a ring
        // of light, not a filled blob.
        bgPaint.color = bg
        bgPaint.alpha = 217
        canvas.drawCircle(cx, cy, coreR * 0.55f, bgPaint)
    }
}
