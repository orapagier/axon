package com.agentapi.android

import com.axon.voice.R
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.media.AudioAttributes
import android.media.RingtoneManager
import android.os.Build
import androidx.core.app.NotificationCompat

/**
 * Fired by AlarmManager.setAlarmClock() when the alarm time arrives.
 *
 * This receiver's only job is to post a high-priority notification whose
 * fullScreenIntent launches AlarmDismissActivity. Android grants fullScreenIntent
 * a special exemption from background activity start restrictions — it WILL
 * appear over the lock screen and over any other foreground app.
 *
 * Sound and vibration are handled entirely inside AlarmDismissActivity using
 * the real Ringtone API (not notification sound), so they play at alarm volume
 * and loop correctly regardless of ringer mode.
 */
class AlarmRingerReceiver : BroadcastReceiver() {

    companion object {
        private const val CHANNEL_ID = "alarm_fullscreen_channel"
        const val NOTIF_ID = 9000

        /**
         * Creates (or recreates) the alarm notification channel.
         *
         * We delete and recreate on every call because Android locks channel
         * settings after first creation and silently ignores updates — if a
         * previous install created the channel without the right sound/priority,
         * the only safe fix is to wipe it and start fresh.
         */
        fun ensureChannel(nm: NotificationManager) {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                nm.deleteNotificationChannel(CHANNEL_ID)

                val alarmUri = RingtoneManager.getDefaultUri(RingtoneManager.TYPE_ALARM)
                val audioAttrs = AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_ALARM)
                    .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                    .build()

                val channel = NotificationChannel(
                    CHANNEL_ID,
                    "Alarms",
                    NotificationManager.IMPORTANCE_HIGH
                ).apply {
                    description          = "AgentAPI alarm notifications"
                    setSound(alarmUri, audioAttrs)
                    enableVibration(true)
                    vibrationPattern     = longArrayOf(0, 500, 500, 500)
                    setBypassDnd(true)
                    lockscreenVisibility = NotificationCompat.VISIBILITY_PUBLIC
                }
                nm.createNotificationChannel(channel)
            }
        }
    }

    override fun onReceive(context: Context, intent: Intent) {
        val label = intent.getStringExtra("label") ?: "Agent Alarm"
        val nm    = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager

        ensureChannel(nm)

        // ── Full-screen intent: wakes screen, shown over lock screen ──────────
        //
        // AlarmDismissActivity handles the actual sound + vibration + dismiss UI.
        val fullScreenIntent = Intent(context, AlarmDismissActivity::class.java).apply {
            putExtra(AlarmDismissActivity.EXTRA_LABEL, label)
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_NO_USER_ACTION)
        }
        val fullScreenPi = PendingIntent.getActivity(
            context,
            label.hashCode(),
            fullScreenIntent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )

        // ── Notification (shown in shade, and as heads-up if screen is on) ────
        val notif = NotificationCompat.Builder(context, CHANNEL_ID)
            .setSmallIcon(android.R.drawable.ic_lock_idle_alarm)
            .setContentTitle("⏰ Alarm")
            .setContentText(label)
            .setPriority(NotificationCompat.PRIORITY_MAX)
            .setCategory(NotificationCompat.CATEGORY_ALARM)
            .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
            .setOngoing(true)           // can't be swiped away while ringing
            .setAutoCancel(false)
            // fullScreenIntent is the key: Android exempts this from background
            // activity start restrictions — fires over lock screen and any app
            .setFullScreenIntent(fullScreenPi, true)
            .setContentIntent(fullScreenPi)
            .build()

        nm.notify(NOTIF_ID, notif)
    }
}
