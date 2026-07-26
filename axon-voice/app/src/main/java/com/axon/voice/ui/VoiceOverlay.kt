package com.axon.voice.ui

/**
 * Bridge from the wake-word service into the on-screen hands-free orb, the
 * Android counterpart of the web dashboard's handsFree overlay. [WakeWordService]
 * drives [phase] as one "Hey Axon" exchange moves through
 * listening -> thinking -> speaking -> idle, and pushes the live mic level
 * during capture; [ChatActivity] observes [listener] to show and animate
 * [VoiceOrbView] while it is in the foreground.
 *
 * It also carries the two orb gestures the other way: a tap on the orb core
 * toggles [paused] (hold/resume the spoken reply and the follow-up window), and
 * a tap outside it closes the exchange. Those land on [controller], which
 * [WakeWordService] implements — the service owns the reply stream and capture
 * loop, so it does the actual pausing/closing.
 *
 * Same design as [ChatFeed]: single listener/controller slots, set for a
 * component's visible/alive lifetime and invoked across threads (each side
 * marshals to the thread it needs). When nobody is registered the calls are
 * cheap no-ops — the orb is a foreground affordance, not always-on.
 */
object VoiceOverlay {
    enum class Phase { IDLE, LISTENING, THINKING, SPEAKING }

    /** The activity's view of the exchange: phase/level updates plus the
     *  paused flag, so the orb can show a "held" state. */
    interface Listener {
        fun onState(phase: Phase, level: Float)
        fun onPaused(paused: Boolean)
    }

    /** The service side of the two orb gestures. */
    interface Controller {
        /** Orb-core tap: hold the reply + follow-up window, or resume them. */
        fun onTogglePause()

        /** Tap outside the core: end the current hands-free exchange (the wake
         *  word keeps listening for the next "Hey Axon"). */
        fun onClose()
    }

    @Volatile
    var listener: Listener? = null

    @Volatile
    var controller: Controller? = null

    @Volatile
    var phase: Phase = Phase.IDLE
        private set

    @Volatile
    var paused: Boolean = false
        private set

    /** Move to a new phase (and reset the metered level — the new phase either
     *  supplies its own via [level] or the orb synthesizes one). */
    fun setPhase(p: Phase) {
        phase = p
        listener?.onState(p, -1f)
    }

    /** Feed a mic RMS sample for the reactive listening orb. Ignored outside
     *  the listening phase, where nothing is being metered. */
    fun level(rms: Float) {
        if (phase == Phase.LISTENING) listener?.onState(Phase.LISTENING, rms)
    }

    /** Feed a reply-audio RMS sample for the reactive speaking orb (from the TTS
     *  player). Ignored outside the speaking phase, so ack playback and other
     *  sounds don't drive the orb — only the reply the user is hearing does. */
    fun speakLevel(rms: Float) {
        if (phase == Phase.SPEAKING) listener?.onState(Phase.SPEAKING, rms)
    }

    /** Reflect the exchange's hold state onto the orb. Driven by the service so
     *  a resume from any source (the tap, a close) keeps the orb in step. */
    fun setPaused(p: Boolean) {
        paused = p
        listener?.onPaused(p)
    }
}
