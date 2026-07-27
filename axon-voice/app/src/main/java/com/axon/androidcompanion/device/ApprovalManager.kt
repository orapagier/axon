package com.axon.androidcompanion.device

import com.axon.androidcompanion.R
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Build
import androidx.core.app.NotificationCompat
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.withTimeoutOrNull
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap

/**
 * Gates destructive, hard-to-undo actions (shell.run, hard deletes, out-of-sandbox file
 * writes) behind a physical notification tap on the device.
 *
 * A voice agent can mishear a command or an LLM tool call can be hallucinated — this makes
 * sure a human is actually looking at the phone before anything irreversible runs, without
 * adding friction to the large majority of read-only / low-risk calls.
 */
object ApprovalManager {

    private const val CHANNEL_ID = "androidcompanion_approvals"
    const val ACTION_APPROVE     = "com.axon.androidcompanion.device.APPROVE"
    const val ACTION_DENY        = "com.axon.androidcompanion.device.DENY"
    const val EXTRA_REQUEST_ID   = "request_id"
    const val EXTRA_NOTIF_ID     = "notif_id"

    private val pending = ConcurrentHashMap<String, CompletableDeferred<Boolean>>()

    private fun ensureChannel(ctx: Context) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(
                CHANNEL_ID, "AndroidCompanion Approvals", NotificationManager.IMPORTANCE_HIGH
            ).apply {
                description = "Approve or deny a destructive action requested by the agent"
                enableVibration(true)
            }
            (ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager)
                .createNotificationChannel(channel)
        }
    }

    /**
     * Suspends until the user taps Approve/Deny on the phone or [timeoutMs] elapses.
     * Returns true only on an explicit Approve tap — timeout and Deny both resolve to false.
     */
    suspend fun requestApproval(ctx: Context, description: String, timeoutMs: Long = 90_000): Boolean {
        ensureChannel(ctx)
        val id       = UUID.randomUUID().toString()
        val notifId  = id.hashCode()
        val deferred = CompletableDeferred<Boolean>()
        pending[id]  = deferred

        showNotification(ctx, id, notifId, description)
        val approved = withTimeoutOrNull(timeoutMs) { deferred.await() } ?: false

        pending.remove(id)
        (ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager).cancel(notifId)
        return approved
    }

    private fun showNotification(ctx: Context, id: String, notifId: Int, description: String) {
        fun actionIntent(action: String) = PendingIntent.getBroadcast(
            ctx, (id + action).hashCode(),
            Intent(ctx, ApprovalReceiver::class.java).apply {
                this.action = action
                putExtra(EXTRA_REQUEST_ID, id)
                putExtra(EXTRA_NOTIF_ID, notifId)
            },
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )

        val notification = NotificationCompat.Builder(ctx, CHANNEL_ID)
            .setSmallIcon(android.R.drawable.ic_dialog_alert)
            .setContentTitle("AndroidCompanion wants to do something irreversible")
            .setContentText(description)
            .setStyle(NotificationCompat.BigTextStyle().bigText(description))
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setCategory(NotificationCompat.CATEGORY_CALL) // heads-up + shows over lock screen
            .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
            .setOngoing(true) // resists accidental swipe-away while a decision is pending
            .addAction(android.R.drawable.ic_menu_send, "Approve", actionIntent(ACTION_APPROVE))
            .addAction(android.R.drawable.ic_menu_close_clear_cancel, "Deny", actionIntent(ACTION_DENY))
            .build()

        (ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager).notify(notifId, notification)
    }

    /** Called by ApprovalReceiver when the user taps Approve or Deny. */
    fun resolve(requestId: String, approved: Boolean) {
        pending.remove(requestId)?.complete(approved)
    }
}
