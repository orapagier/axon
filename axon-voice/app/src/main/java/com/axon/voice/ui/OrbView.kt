package com.axon.voice.ui

import android.animation.ValueAnimator
import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.RectF
import android.util.AttributeSet
import android.view.View
import android.view.animation.LinearInterpolator
import androidx.core.content.ContextCompat
import com.axon.voice.R
import kotlin.math.PI
import kotlin.math.min
import kotlin.math.sin

/**
 * The voice orb: tap target and state display in one. Idle is a dim disc;
 * listening pulses with an expanding ring; thinking spins an arc; speaking
 * radiates rings.
 */
class OrbView @JvmOverloads constructor(
    context: Context,
    attrs: AttributeSet? = null,
) : View(context, attrs) {

    enum class OrbState { IDLE, LISTENING, THINKING, SPEAKING }

    var orbState: OrbState = OrbState.IDLE
        set(value) {
            field = value
            invalidate()
        }

    private val accent = ContextCompat.getColor(context, R.color.accent)
    private val accentDim = ContextCompat.getColor(context, R.color.accent_dim)

    private val fill = Paint(Paint.ANTI_ALIAS_FLAG)
    private val ring = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeWidth = 6f
        strokeCap = Paint.Cap.ROUND
    }
    private val arcRect = RectF()

    private var phase = 0f
    private val animator = ValueAnimator.ofFloat(0f, 1f).apply {
        duration = 1600
        repeatCount = ValueAnimator.INFINITE
        interpolator = LinearInterpolator()
        addUpdateListener {
            phase = it.animatedValue as Float
            if (orbState != OrbState.IDLE) invalidate()
        }
    }

    override fun onAttachedToWindow() {
        super.onAttachedToWindow()
        animator.start()
    }

    override fun onDetachedFromWindow() {
        animator.cancel()
        super.onDetachedFromWindow()
    }

    override fun onDraw(canvas: Canvas) {
        val cx = width / 2f
        val cy = height / 2f
        val base = min(width, height) / 2f * 0.55f

        when (orbState) {
            OrbState.IDLE -> {
                fill.color = accentDim
                canvas.drawCircle(cx, cy, base, fill)
            }

            OrbState.LISTENING -> {
                val breathe = 1f + 0.05f * sin(2.0 * PI * phase).toFloat()
                fill.color = accent
                canvas.drawCircle(cx, cy, base * breathe, fill)
                ring.color = accent
                ring.alpha = ((1f - phase) * 200).toInt()
                canvas.drawCircle(cx, cy, base * (1f + 0.5f * phase), ring)
            }

            OrbState.THINKING -> {
                fill.color = accentDim
                canvas.drawCircle(cx, cy, base, fill)
                ring.color = accent
                ring.alpha = 255
                val r = base * 1.28f
                arcRect.set(cx - r, cy - r, cx + r, cy + r)
                canvas.drawArc(arcRect, phase * 360f, 100f, false, ring)
            }

            OrbState.SPEAKING -> {
                fill.color = accent
                canvas.drawCircle(cx, cy, base, fill)
                ring.color = accent
                for (offset in listOf(0f, 0.5f)) {
                    val p = (phase + offset) % 1f
                    ring.alpha = ((1f - p) * 160).toInt()
                    canvas.drawCircle(cx, cy, base * (1f + 0.5f * p), ring)
                }
            }
        }
    }
}
