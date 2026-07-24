package com.axon.voice.ui

/**
 * Bridge from the wake-word service into the on-screen hands-free orb, the
 * Android counterpart of the web dashboard's handsFree overlay. [WakeWordService]
 * drives [phase] as one "Hey Axon" exchange moves through
 * listening -> thinking -> speaking -> idle, and pushes the live mic level
 * during capture; [ChatActivity] observes [listener] to show and animate
 * [VoiceOrbView] while it is in the foreground.
 *
 * Same design as [ChatFeed]: a single listener slot, set by the activity for
 * its visible lifetime and invoked from service threads (the activity marshals
 * to the main thread). When no activity is listening the calls are cheap no-ops
 * — the orb is a foreground affordance, not always-on.
 */
object VoiceOverlay {
    enum class Phase { IDLE, LISTENING, THINKING, SPEAKING }

    fun interface Listener {
        fun onState(phase: Phase, level: Float)
    }

    @Volatile
    var listener: Listener? = null

    @Volatile
    var phase: Phase = Phase.IDLE
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
}
