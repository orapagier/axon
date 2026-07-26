package com.agentapi.android

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.provider.Telephony
import android.util.Log

/**
 * Fires when an SMS arrives. Immediately pushes to axon-agent's webhook.
 * This is event-driven — no polling needed from axon-agent's side.
 */
class SmsReceiver : BroadcastReceiver() {

    private val TAG = "SmsReceiver"

    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Telephony.Sms.Intents.SMS_RECEIVED_ACTION) return

        val messages = Telephony.Sms.Intents.getMessagesFromIntent(intent)
        for (msg in messages) {
            val from = msg.originatingAddress ?: "unknown"
            val body = msg.messageBody ?: ""
            val ts   = msg.timestampMillis

            Log.i(TAG, "SMS from $from — pushing to webhook")
            WebhookPusher.pushSms(context, from, body, ts)
        }
    }
}
