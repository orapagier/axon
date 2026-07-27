package com.axon.androidcompanion.device

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.telephony.TelephonyManager
import android.util.Log

/**
 * Monitors call state changes and pushes events to axon-agent.
 *
 * Events fired:
 *   "call_incoming"  — phone is ringing (number + name if available)
 *   "call_missed"    — incoming call that was not answered
 *
 * State machine:
 *   IDLE → RINGING  = incoming call
 *   RINGING → IDLE  = missed (never went to OFFHOOK)
 *   RINGING → OFFHOOK = answered (no push)
 *   OFFHOOK → IDLE  = call ended (no push)
 */
class CallStateReceiver : BroadcastReceiver() {

    private val TAG = "CallStateReceiver"

    companion object {
        // We need to track the previous state to detect missed calls.
        // Static field survives across receiver instantiations (BroadcastReceivers are
        // created fresh each time, so instance fields won't work).
        @Volatile private var ringingNumber   = ""
        @Volatile private var wasRinging      = false
    }

    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != TelephonyManager.ACTION_PHONE_STATE_CHANGED) return

        val state  = intent.getStringExtra(TelephonyManager.EXTRA_STATE) ?: return
        
        // EXTRA_INCOMING_NUMBER is deprecated for privacy reasons in newer APIs.
        // It requires READ_CALL_LOG and READ_PHONE_STATE permissions to be populated.
        @Suppress("DEPRECATION")
        val number = intent.getStringExtra(TelephonyManager.EXTRA_INCOMING_NUMBER) ?: ""

        Log.d(TAG, "Call state: $state  number: $number")

        when (state) {
            TelephonyManager.EXTRA_STATE_RINGING -> {
                wasRinging    = true
                ringingNumber = number
                Log.i(TAG, "Incoming call from $number")
                WebhookPusher.pushCall(context, "call_incoming", number, null)
            }

            TelephonyManager.EXTRA_STATE_OFFHOOK -> {
                // Call was answered — nothing to push, reset flag
                wasRinging = false
            }

            TelephonyManager.EXTRA_STATE_IDLE -> {
                if (wasRinging) {
                    // Went from RINGING directly to IDLE = missed call
                    Log.i(TAG, "Missed call from $ringingNumber")
                    WebhookPusher.pushCall(context, "call_missed", ringingNumber, null)
                    wasRinging    = false
                    ringingNumber = ""
                }
            }
        }
    }
}
