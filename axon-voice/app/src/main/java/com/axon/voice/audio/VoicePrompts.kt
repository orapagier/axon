package com.axon.voice.audio

import kotlin.random.Random

/**
 * Short spoken prompts for the wake flow — the Kotlin mirror of
 * `axon-ui/src/lib/voiceprompts.js`: the wake ack is picked uniformly at
 * random from [WAKE_ACKS] so each wake sounds fresh, and a single
 * [FOLLOWUP_PROMPT] announces the follow-up window.
 *
 * [THINKING_FILLERS] has no dashboard counterpart (the dashboard only shows
 * a visual "Thinking..." label). The Android client speaks one while the
 * agent is processing, to make the round-trip feel conversational.
 */
object VoicePrompts {

    /** Spoken right after "Hey Axon" fires. Mirrors WAKE_ACKS in voiceprompts.js. */
    val WAKE_ACKS: List<String> = listOf("Yes?", "Mm-hmm?", "I'm listening.")

    /** Fixed phrase that opens the follow-up window. Mirrors FOLLOWUP_PROMPT. */
    const val FOLLOWUP_PROMPT: String = "Anything else?"

    /** One is picked at random and spoken while the agent processes a command. */
    val THINKING_FILLERS: List<String> = listOf(
        "Let me check.", "Working on it.", "One sec.", "On it.",
    )

    private val rng = Random.Default

    fun randomWakeAck(): String = WAKE_ACKS[rng.nextInt(WAKE_ACKS.size)]

    fun randomFiller(): String = THINKING_FILLERS[rng.nextInt(THINKING_FILLERS.size)]

    /** Every phrase that needs a prefetched server-TTS audio file. Fillers are
     *  included: with no cached file they fall back to the device's built-in
     *  engine, which sounds nothing like the configured tts.* voice the replies
     *  and acks are spoken in. */
    val allPrefetchable: List<String> = WAKE_ACKS + FOLLOWUP_PROMPT + THINKING_FILLERS
}
