package com.axon.voice.audio

import kotlin.random.Random

/**
 * Short spoken prompts for the wake flow — the Kotlin mirror of
 * `axon-ui/src/lib/voiceprompts.js`: the wake ack is picked uniformly at
 * random from [WAKE_ACKS] so each wake sounds fresh.
 *
 * The acks are all that is left on purpose. Earlier versions also spoke a
 * rotating thinking filler ("Let me check.", "On it."…) while the agent
 * worked and "Anything else?" to open the follow-up window; a stock phrase on
 * every turn read as chatter rather than conversation. The follow-up window is
 * announced by its soft chime alone, and a working agent stays quiet — that
 * silence is intended, not a gap to fill.
 */
object VoicePrompts {

    /** Spoken right after "Hey Axon" fires. Mirrors WAKE_ACKS in voiceprompts.js.
     *  Every phrase must read as a word, not spelled-out letters: Piper's
     *  espeak-ng turns "Mm-hmm?" into "em-em-hum", so the affirmative here is
     *  "Uh-huh?" (which every engine, hosted or local, says naturally). */
    val WAKE_ACKS: List<String> = listOf("Yes?", "Uh-huh?", "I'm listening.")

    private val rng = Random.Default

    fun randomWakeAck(): String = WAKE_ACKS[rng.nextInt(WAKE_ACKS.size)]

    /** Every phrase that needs a prefetched server-TTS audio file: with no
     *  cached file they fall back to the device's built-in engine, which sounds
     *  nothing like the configured tts.* voice the replies are spoken in. */
    val allPrefetchable: List<String> = WAKE_ACKS
}
