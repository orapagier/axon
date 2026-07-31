package com.axon.androidcompanion.device

import android.content.Context
import android.media.AudioManager

/**
 * The one signal the hands-free path needs from telephony: "this device is
 * about to be — or already is — in a phone call, so stop listening."
 *
 * A spoken "call Mom" runs device_tool's call op, which lands in
 * [ApiServer.initiateCall] on this very phone. Every path there reports through
 * [markCallPlaced], so the wake service learns about the call the moment it is
 * dialed rather than inferring it from the agent's reply text — the reply is
 * free-form English, the dial is not.
 *
 * Two complementary signals, because they cover different windows:
 *  - [markCallPlaced] fires instantly, while the agent's "Calling Mom" reply is
 *    still being spoken, so the exchange can be closed before its follow-up
 *    window would open the mic onto the call.
 *  - [inCall] is a poll of the audio mode, true for the whole call (including
 *    incoming ones the user answered, which nothing here places). It keeps the
 *    always-on wake capture off the air for the call's duration, so a
 *    false-positive "Hey Axon" can't start recording the conversation either.
 *
 * Nothing in this app sets [AudioManager.setMode], so the mode is purely the
 * telephony/VoIP stack's, and it needs no permission to read — unlike
 * TelephonyManager.getCallState, which requires READ_PHONE_STATE from API 31.
 */
object CallGuard {

    /** Implemented by the wake service so a placed call reaches it immediately,
     *  without polling. Set for the service's lifetime. */
    interface Listener {
        /** A call was just placed on this device (any device_tool call path). */
        fun onCallPlaced()
    }

    @Volatile
    var listener: Listener? = null

    /** Report a call being dialed on this device. Called from whichever thread
     *  placed it (the API server's request thread); the listener does only
     *  volatile-flag work, so it is safe to run inline here. */
    fun markCallPlaced() {
        listener?.onCallPlaced()
    }

    /** True while the device is in a phone call (or a VoIP call routed through
     *  the communication mode). Deliberately excludes MODE_RINGTONE: a ringing
     *  phone is not yet a conversation, and the user may well want to say
     *  something to Axon about it. */
    fun inCall(ctx: Context): Boolean {
        val am = ctx.getSystemService(Context.AUDIO_SERVICE) as? AudioManager ?: return false
        val mode = am.mode
        return mode == AudioManager.MODE_IN_CALL || mode == AudioManager.MODE_IN_COMMUNICATION
    }
}
