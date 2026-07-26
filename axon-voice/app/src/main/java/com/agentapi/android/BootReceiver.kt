package com.agentapi.android

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log
import androidx.core.content.ContextCompat

/**
 * Starts ApiService on boot and after app updates.
 * Also doubles as the target for the AlarmManager backup restart
 * scheduled in ApiService.onDestroy() / onTaskRemoved().
 */
class BootReceiver : BroadcastReceiver() {

    private val TAG = "BootReceiver"

    override fun onReceive(context: Context, intent: Intent) {
        Log.i(TAG, "Received: ${intent.action}")
        val serviceIntent = Intent(context, ApiService::class.java)
            .apply { action = ApiService.ACTION_START }
        ContextCompat.startForegroundService(context, serviceIntent)
    }
}
