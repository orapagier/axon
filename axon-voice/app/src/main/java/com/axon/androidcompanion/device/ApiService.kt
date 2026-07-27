package com.axon.androidcompanion.device

import com.axon.androidcompanion.R
import com.axon.androidcompanion.ui.SettingsActivity
import android.app.AlarmManager
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.location.Location
import android.os.BatteryManager
import android.os.Build
import android.os.IBinder
import android.os.Looper
import android.os.PowerManager
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import androidx.work.*
import com.google.android.gms.location.*
import java.net.NetworkInterface
import java.util.Collections
import java.util.concurrent.TimeUnit

class ApiService : Service() {

    companion object {
        private const val TAG              = "ApiService"
        private const val CHANNEL_ID      = "androidcompanion_service"
        // Distinct from axon-voice's WakeWordService.NOTIF_ID (=1) — notification IDs
        // are scoped per-package, not per-channel, so these must not collide with it.
        private const val NOTIF_ID        = 101
        private const val NOTIF_ID_EVENT  = 102

        const val ACTION_START   = "com.axon.androidcompanion.device.START"
        const val ACTION_STOP    = "com.axon.androidcompanion.device.STOP"

        // Static state — accessed by ApiServer, MainActivity, receivers
        @Volatile var isRunning   = false
        @Volatile private var cachedLocation: Location? = null

        fun getCachedLocation() = cachedLocation

        fun getLocalIp(): String = try {
            NetworkInterface.getNetworkInterfaces()?.asSequence()
                ?.filter { it.isUp && !it.isLoopback }
                ?.flatMap { it.inetAddresses.asSequence() }
                ?.filterIsInstance<java.net.Inet4Address>()
                ?.firstOrNull()?.hostAddress ?: "127.0.0.1"
        } catch (e: Exception) { "127.0.0.1" }
    }

    private var apiServer: ApiServer?                   = null
    private var fusedLocationClient: FusedLocationProviderClient? = null
    private var locationCallback: LocationCallback?     = null
    private var wakeLock: PowerManager.WakeLock?        = null

    // ── Lifecycle ─────────────────────────────────────────────────────────────

    override fun onCreate() {
        super.onCreate()
        createNotificationChannel()

        // PARTIAL_WAKE_LOCK: keeps CPU alive for HTTP requests even when screen off
        val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
        wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "AndroidCompanion::WakeLock")
        wakeLock?.setReferenceCounted(false)
        wakeLock?.acquire()

        fusedLocationClient = LocationServices.getFusedLocationProviderClient(this)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> { shutdown(); return START_NOT_STICKY }
            else        -> {
                // startForeground() MUST be called within a few seconds of
                // startForegroundService() or the OS kills the app with
                // ForegroundServiceDidNotStartInTimeException. Do it first, before any
                // slower init (Keystore-backed token decrypt, Ktor/Netty bootstrap).
                startForegroundCompat()
                startup()  // ACTION_START, null (restarted by OS), anything else
            }
        }
        return START_STICKY  // OS will restart us if killed, passing null intent
    }

    override fun onBind(intent: Intent?): IBinder? = null

    /**
     * User swiped the app away from Recents.
     * stopWithTask="false" in manifest means onTaskRemoved() fires but the service
     * is NOT stopped. This is just a signal; we schedule an AlarmManager backup anyway.
     */
    override fun onTaskRemoved(rootIntent: Intent?) {
        Log.w(TAG, "Task removed — scheduling AlarmManager backup restart")
        scheduleAlarmRestart()
        super.onTaskRemoved(rootIntent)
    }

    override fun onDestroy() {
        Log.w(TAG, "onDestroy — scheduling restart")
        scheduleAlarmRestart()
        stopLocationUpdates()
        wakeLock?.takeIf { it.isHeld }?.release()
        super.onDestroy()
    }

    // ── Startup / Shutdown ────────────────────────────────────────────────────

    private fun startup() {
        if (isRunning) {
            Log.d(TAG, "Already running")
            return
        }
        try {
            apiServer = ApiServer(this).also { it.start() }
            isRunning = true
            startLocationUpdates()
            scheduleWatchdog()
            CloudflaredManager.start(this) // no-op if no tunnel token is configured yet
            Log.i(TAG, "AndroidCompanion started — http://127.0.0.1:${AppConfig.getPort(this)} (loopback only)")
        } catch (e: Exception) {
            Log.e(TAG, "Startup failed", e)
            stopSelf()
        }
    }

    private fun shutdown() {
        stopLocationUpdates()
        CloudflaredManager.stop()
        WebhookPusher.shutdown()
        apiServer?.stop()
        apiServer = null
        isRunning = false
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    // ── Foreground notification ────────────────────────────────────────────────

    private fun startForegroundCompat() {
        val notification = buildNotification()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            var type = ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC
            val hasLocation = ContextCompat.checkSelfPermission(this,
                android.Manifest.permission.ACCESS_FINE_LOCATION) == PackageManager.PERMISSION_GRANTED
                    || ContextCompat.checkSelfPermission(this,
                android.Manifest.permission.ACCESS_COARSE_LOCATION) == PackageManager.PERMISSION_GRANTED
            if (hasLocation) type = type or ServiceInfo.FOREGROUND_SERVICE_TYPE_LOCATION
            startForeground(NOTIF_ID, notification, type)
        } else {
            startForeground(NOTIF_ID, notification)
        }
    }

    private fun createNotificationChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(CHANNEL_ID, "AndroidCompanion Service",
                NotificationManager.IMPORTANCE_LOW).apply {
                description = "AndroidCompanion HTTP server"
                setShowBadge(false)
                enableVibration(false)
            }
            getSystemService(NotificationManager::class.java)?.createNotificationChannel(channel)
        }
    }

    private fun buildNotification(): Notification {
        val mainIntent = PendingIntent.getActivity(
            this, 0, Intent(this, SettingsActivity::class.java),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        val stopIntent = PendingIntent.getService(
            this, 1, Intent(this, ApiService::class.java).apply { action = ACTION_STOP },
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        val port = AppConfig.getPort(this)
        val tunnelNote = if (CloudflaredManager.isRunning()) " · tunnel ●" else ""
        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle("AndroidCompanion Running")
            .setContentText("http://127.0.0.1:$port (device-only)$tunnelNote")
            .setSmallIcon(android.R.drawable.ic_dialog_info)
            .setContentIntent(mainIntent)
            .addAction(android.R.drawable.ic_delete, "Stop", stopIntent)
            .setOngoing(true)
            .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .build()
    }

    // ── Location ──────────────────────────────────────────────────────────────

    private fun startLocationUpdates() {
        val hasFine = ContextCompat.checkSelfPermission(this,
            android.Manifest.permission.ACCESS_FINE_LOCATION) == PackageManager.PERMISSION_GRANTED
        val hasCoarse = ContextCompat.checkSelfPermission(this,
            android.Manifest.permission.ACCESS_COARSE_LOCATION) == PackageManager.PERMISSION_GRANTED

        if (!hasFine && !hasCoarse) {
            Log.w(TAG, "No location permission — skipping location updates")
            return
        }

        val priority = if (hasFine) Priority.PRIORITY_BALANCED_POWER_ACCURACY
                       else Priority.PRIORITY_LOW_POWER

        val request = LocationRequest.Builder(priority, 5 * 60_000L)
            .setMinUpdateIntervalMillis(60_000L)
            .setWaitForAccurateLocation(false)
            .build()

        var lastPushedLocation: Location? = null

        locationCallback = object : LocationCallback() {
            override fun onLocationResult(result: LocationResult) {
                val loc = result.lastLocation ?: return
                cachedLocation = loc

                // Push to axon-agent only when moved meaningfully (>50m) to avoid spam
                val shouldPush = lastPushedLocation == null
                    || loc.distanceTo(lastPushedLocation!!) > 50f
                if (shouldPush) {
                    WebhookPusher.pushLocation(
                        this@ApiService,
                        loc.latitude, loc.longitude,
                        loc.accuracy, loc.provider ?: "unknown"
                    )
                    lastPushedLocation = loc
                }
            }
        }

        try {
            fusedLocationClient?.requestLocationUpdates(
                request, locationCallback!!, Looper.getMainLooper())
            Log.i(TAG, "Location updates started")
        } catch (e: SecurityException) {
            Log.e(TAG, "Location security exception", e)
        }
    }

    private fun stopLocationUpdates() {
        locationCallback?.let { fusedLocationClient?.removeLocationUpdates(it) }
        locationCallback = null
    }

    // ── Persistence: AlarmManager backup + WorkManager watchdog ──────────────

    /**
     * Schedules a one-shot alarm 3 seconds from now.
     * RestartReceiver picks it up and re-fires the service.
     * Covers the case where START_STICKY is delayed by the OS.
     */
    private fun scheduleAlarmRestart() {
        val pi = PendingIntent.getBroadcast(
            applicationContext, 42,
            Intent(applicationContext, BootReceiver::class.java).apply { action = ACTION_START },
            PendingIntent.FLAG_ONE_SHOT or PendingIntent.FLAG_IMMUTABLE
        )
        val am = getSystemService(Context.ALARM_SERVICE) as AlarmManager
        val at = System.currentTimeMillis() + 3_000
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
            am.setExactAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, at, pi)
        } else {
            am.set(AlarmManager.RTC_WAKEUP, at, pi)
        }
    }

    /**
     * WorkManager periodic task — runs every 15 minutes (minimum WorkManager interval).
     * If ApiService is dead, restarts it. Survives Doze, App Standby, and most OEM killers.
     */
    private fun scheduleWatchdog() {
        val work = PeriodicWorkRequestBuilder<WatchdogWorker>(15, TimeUnit.MINUTES)
            .setConstraints(Constraints.Builder()
                .setRequiredNetworkType(NetworkType.NOT_REQUIRED)
                .build())
            .build()
        WorkManager.getInstance(this).enqueueUniquePeriodicWork(
            "androidcompanion_watchdog",
            ExistingPeriodicWorkPolicy.KEEP,
            work
        )
        Log.i(TAG, "WorkManager watchdog scheduled")
    }
}

// ── WorkManager Watchdog ──────────────────────────────────────────────────────

/**
 * Runs every 15 minutes. If ApiService isn't running, starts it.
 * WorkManager jobs survive reboots and OEM battery killers better than
 * AlarmManager alone, because WorkManager registers a JobScheduler job
 * under the hood.
 */
class WatchdogWorker(ctx: Context, params: WorkerParameters) : Worker(ctx, params) {
    override fun doWork(): Result {
        if (!AppConfig.isDeviceControlEnabled(applicationContext)) {
            return Result.success() // user switched it off — don't resurrect it
        }
        if (!ApiService.isRunning) {
            Log.w("WatchdogWorker", "ApiService dead — restarting")
            val intent = Intent(applicationContext, ApiService::class.java)
                .apply { action = ApiService.ACTION_START }
            ContextCompat.startForegroundService(applicationContext, intent)
        } else if (!CloudflaredManager.isRunning() &&
            AppConfig.getCloudflaredToken(applicationContext).isNotBlank()) {
            Log.w("WatchdogWorker", "cloudflared dead but a token is configured — restarting tunnel")
            CloudflaredManager.start(applicationContext)
        }
        return Result.success()
    }
}
