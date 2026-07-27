package com.axon.androidcompanion.device

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.BatteryManager
import android.util.Log

/**
 * Registered dynamically in ApiService (not in manifest) because
 * ACTION_BATTERY_LOW and ACTION_BATTERY_CHANGED cannot be received by
 * manifest-declared receivers on API 26+ — they require dynamic registration.
 *
 * ApiService registers this in onCreate() and unregisters in onDestroy().
 *
 * Pushes a "battery_low" event to axon-agent when battery drops to/below threshold.
 */
class BatteryReceiver : BroadcastReceiver() {

    private val TAG = "BatteryReceiver"

    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Intent.ACTION_BATTERY_CHANGED) return

        val level   = intent.getIntExtra(BatteryManager.EXTRA_LEVEL, -1)
        val scale   = intent.getIntExtra(BatteryManager.EXTRA_SCALE, -1)
        val status  = intent.getIntExtra(BatteryManager.EXTRA_STATUS, -1)
        val plugged = intent.getIntExtra(BatteryManager.EXTRA_PLUGGED, -1)

        if (scale <= 0) return
        val percent  = level * 100 / scale
        val charging = status == BatteryManager.BATTERY_STATUS_CHARGING
                    || status == BatteryManager.BATTERY_STATUS_FULL
        val pluggedStr = when (plugged) {
            BatteryManager.BATTERY_PLUGGED_AC       -> "AC"
            BatteryManager.BATTERY_PLUGGED_USB      -> "USB"
            BatteryManager.BATTERY_PLUGGED_WIRELESS -> "wireless"
            else -> "unplugged"
        }

        val threshold = AppConfig.getBatteryThreshold(context)

        // Push only when: below threshold AND not charging AND not already pushed recently
        if (percent <= threshold && !charging) {
            Log.i(TAG, "Battery low ($percent%) — pushing to webhook")
            WebhookPusher.pushBattery(context, percent, false, pluggedStr)
        }
    }
}
