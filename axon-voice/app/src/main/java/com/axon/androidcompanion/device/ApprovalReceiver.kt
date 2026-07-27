package com.axon.androidcompanion.device

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.app.NotificationManager

/** Handles the Approve/Deny taps on the notification pushed by ApprovalManager. */
class ApprovalReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val id = intent.getStringExtra(ApprovalManager.EXTRA_REQUEST_ID) ?: return
        val approved = intent.action == ApprovalManager.ACTION_APPROVE
        ApprovalManager.resolve(id, approved)

        val notifId = intent.getIntExtra(ApprovalManager.EXTRA_NOTIF_ID, -1)
        if (notifId != -1) {
            (context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager).cancel(notifId)
        }
    }
}
