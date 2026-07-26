package com.agentapi.android

import com.axon.voice.R
import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.provider.Settings
import android.widget.Toast
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AlertDialog
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.ContextCompat
import android.os.Environment
import android.provider.Settings as AndroidSettings
import com.google.android.material.switchmaterial.SwitchMaterial
import com.google.android.material.textfield.TextInputEditText
import com.google.android.material.button.MaterialButton
import android.widget.TextView

/**
 * Setup and status screen.
 *
 * Shows:
 *  - Server status + URL
 *  - Bearer token (tap to copy)
 *  - Webhook URL config
 *  - Push event toggles
 *  - Auto-answer toggle
 *  - Permission status
 *  - Termux status
 *  - Cloudflared setup instructions
 */
class DeviceApiActivity : AppCompatActivity() {

    private val handler       = Handler(Looper.getMainLooper())
    private val statusUpdater = object : Runnable {
        override fun run() { refreshStatus(); handler.postDelayed(this, 2000) }
    }

    // Views — using direct findViewById; you can migrate to ViewBinding if preferred
    private lateinit var tvStatus       : TextView
    private lateinit var tvUrl          : TextView
    private lateinit var tvToken        : TextView
    private lateinit var tvTermux       : TextView
    private lateinit var tvPermissions  : TextView
    private lateinit var tvTunnelStatus : TextView
    private lateinit var etWebhook      : TextInputEditText
    private lateinit var etCloudflaredToken      : TextInputEditText
    private lateinit var btnSaveCloudflaredToken : MaterialButton
    private lateinit var btnToggle      : MaterialButton
    private lateinit var btnSaveWebhook : MaterialButton
    private lateinit var btnPermissions : MaterialButton
    private lateinit var btnBatteryOpt  : MaterialButton
    private lateinit var btnAllFiles    : MaterialButton
    private lateinit var btnWriteSettings   : MaterialButton
    private lateinit var btnOverlayPerm     : MaterialButton
    private lateinit var swAutoAnswer   : SwitchMaterial
    private lateinit var swPushSms      : SwitchMaterial
    private lateinit var swPushCalls    : SwitchMaterial
    private lateinit var swPushLocation : SwitchMaterial
    private lateinit var swPushBattery  : SwitchMaterial

    private val permissionLauncher = registerForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions()
    ) { refreshStatus() }

    // ── Lifecycle ─────────────────────────────────────────────────────────────

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_device_api)

        bindViews()
        loadConfig()
        requestMissingPermissions()
        promptBatteryOptimization()
        promptAllFilesAccess()
        promptWriteSettings()
        promptOverlayPerm()
    }

    override fun onResume() {
        super.onResume()
        refreshStatus()
        handler.post(statusUpdater)
    }

    override fun onPause() {
        super.onPause()
        handler.removeCallbacks(statusUpdater)
    }

    // ── View binding ──────────────────────────────────────────────────────────

    private fun bindViews() {
        tvStatus       = findViewById(R.id.tvStatus)
        tvUrl          = findViewById(R.id.tvUrl)
        tvToken        = findViewById(R.id.tvToken)
        tvTermux       = findViewById(R.id.tvTermux)
        tvPermissions  = findViewById(R.id.tvPermissions)
        tvTunnelStatus = findViewById(R.id.tvTunnelStatus)
        etWebhook      = findViewById(R.id.etWebhook)
        etCloudflaredToken      = findViewById(R.id.etCloudflaredToken)
        btnSaveCloudflaredToken = findViewById(R.id.btnSaveCloudflaredToken)
        btnToggle      = findViewById(R.id.btnToggle)
        btnSaveWebhook = findViewById(R.id.btnSaveWebhook)
        btnPermissions = findViewById(R.id.btnPermissions)
        btnBatteryOpt  = findViewById(R.id.btnBatteryOpt)
        btnAllFiles    = findViewById(R.id.btnAllFiles)
        btnWriteSettings = findViewById(R.id.btnWriteSettings)
        btnOverlayPerm   = findViewById(R.id.btnOverlayPerm)
        swAutoAnswer   = findViewById(R.id.swAutoAnswer)
        swPushSms      = findViewById(R.id.swPushSms)
        swPushCalls    = findViewById(R.id.swPushCalls)
        swPushLocation = findViewById(R.id.swPushLocation)
        swPushBattery  = findViewById(R.id.swPushBattery)

        btnToggle.setOnClickListener { toggleServer() }

        btnSaveWebhook.setOnClickListener {
            val url = etWebhook.text.toString().trim()
            AppConfig.setWebhookUrl(this, url)
            Toast.makeText(this, "Webhook URL saved", Toast.LENGTH_SHORT).show()
        }

        btnSaveCloudflaredToken.setOnClickListener {
            val tok = etCloudflaredToken.text.toString().trim()
            if (tok.isNotEmpty()) {
                AppConfig.setCloudflaredToken(this, tok)
                etCloudflaredToken.setText("")
                CloudflaredManager.start(this)
                Toast.makeText(this, "Tunnel token saved — restarting tunnel", Toast.LENGTH_SHORT).show()
                handler.postDelayed({ refreshStatus() }, 1500)
            } else {
                Toast.makeText(this, "Enter a token first", Toast.LENGTH_SHORT).show()
            }
        }

        btnPermissions.setOnClickListener { requestMissingPermissions() }

        btnBatteryOpt.setOnClickListener { openBatteryOptimizationSettings() }

        btnAllFiles.setOnClickListener { openAllFilesAccessSettings() }

        btnWriteSettings.setOnClickListener { openWriteSettingsSettings() }

        btnOverlayPerm.setOnClickListener { openOverlayPermSettings() }

        // Token tap → copy to clipboard
        tvToken.setOnClickListener {
            val token = AppConfig.getBearerToken(this)
            val cm = getSystemService(CLIPBOARD_SERVICE) as android.content.ClipboardManager
            cm.setPrimaryClip(android.content.ClipData.newPlainText("Bearer Token", token))
            Toast.makeText(this, "Token copied!", Toast.LENGTH_SHORT).show()
        }

        swAutoAnswer.setOnCheckedChangeListener { _, checked ->
            AppConfig.setAutoAnswer(this, checked)
            if (checked && !isAccessibilityServiceEnabled()) {
                showAccessibilityDialog()
            }
        }

        swPushSms.setOnCheckedChangeListener      { _, v -> AppConfig.setPushSms(this, v) }
        swPushCalls.setOnCheckedChangeListener    { _, v -> AppConfig.setPushCalls(this, v) }
        swPushLocation.setOnCheckedChangeListener { _, v -> AppConfig.setPushLocation(this, v) }
        swPushBattery.setOnCheckedChangeListener  { _, v -> AppConfig.setPushBattery(this, v) }
    }

    // ── Load config into UI ───────────────────────────────────────────────────

    private fun loadConfig() {
        etWebhook.setText(AppConfig.getWebhookUrl(this))
        swAutoAnswer.isChecked   = AppConfig.isAutoAnswerEnabled(this)
        swPushSms.isChecked      = AppConfig.isPushSmsEnabled(this)
        swPushCalls.isChecked    = AppConfig.isPushCallsEnabled(this)
        swPushLocation.isChecked = AppConfig.isPushLocationEnabled(this)
        swPushBattery.isChecked  = AppConfig.isPushBatteryEnabled(this)
    }

    // ── Status refresh ────────────────────────────────────────────────────────

    private fun refreshStatus() {
        val running = ApiService.isRunning
        val port    = AppConfig.getPort(this)
        val token   = AppConfig.getBearerToken(this)

        tvStatus.text = if (running) "● Running" else "○ Stopped"
        tvStatus.setTextColor(getColor(if (running) R.color.status_running else R.color.status_stopped))

        // Loopback-only — reachable on this device (or via the Cloudflare Tunnel), not over LAN.
        tvUrl.text = if (running) "Local: http://127.0.0.1:$port (device-only)\nAdd Bearer token to all requests" else "—"
        tvToken.text = "Token (tap to copy): ${token.take(8)}…${token.takeLast(4)}"

        val tunnelConfigured = AppConfig.getCloudflaredToken(this).isNotBlank()
        tvTunnelStatus.text = when {
            !tunnelConfigured           -> "○ Not configured — paste a token below"
            CloudflaredManager.isRunning() -> "● Tunnel running"
            else                         -> "○ Tunnel token set but not running — check jniLibs binary exists for this device's ABI"
        }

        // Termux
        val termuxPath = ApiServer.findTermuxBash()
        tvTermux.text = if (termuxPath != null) "Termux ✓  $termuxPath"
                        else "Termux ✗  Not found (install from F-Droid)"

        // Permissions
        val missing = getMissingPermissions()
        tvPermissions.text = if (missing.isEmpty()) "✓ All permissions granted"
                             else "✗ Missing: ${missing.joinToString { it.substringAfterLast('.') }}"

        btnToggle.text = if (running) "Stop Server" else "Start Server"

        // Battery optimization
        val pm = getSystemService(POWER_SERVICE) as PowerManager
        btnBatteryOpt.text = if (pm.isIgnoringBatteryOptimizations(packageName))
            "Battery Optimization: Disabled ✓"
        else
            "Disable Battery Optimization ⚠"

        // All Files Access (MANAGE_EXTERNAL_STORAGE) — required for Termux shell output
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            btnAllFiles.text = if (Environment.isExternalStorageManager())
                "All Files Access: Granted ✓"
            else
                "Grant All Files Access ⚠  (required for shell.run)"
        } else {
            btnAllFiles.text = "All Files Access: Not required on this Android version"
        }

        // WRITE_SETTINGS — required for brightness control
        btnWriteSettings.text = if (Settings.System.canWrite(this))
            "Modify System Settings: Granted ✓"
        else
            "Grant Modify System Settings ⚠  (required for brightness)"

        // SYSTEM_ALERT_WINDOW — required for launch.url / launch.app on Android 10+
        btnOverlayPerm.text = if (AndroidSettings.canDrawOverlays(this))
            "Draw Over Other Apps: Granted ✓"
        else
            "Grant Draw Over Other Apps ⚠  (required for launch.url/app)"
    }

    // ── Server toggle ──────────────────────────────────────────────────────────

    private fun toggleServer() {
        val intent = Intent(this, ApiService::class.java)
        if (ApiService.isRunning) {
            intent.action = ApiService.ACTION_STOP
            startService(intent)
        } else {
            intent.action = ApiService.ACTION_START
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                startForegroundService(intent)
            } else {
                startService(intent)
            }
        }
        handler.postDelayed({ refreshStatus() }, 800)
    }

    // ── Permissions ───────────────────────────────────────────────────────────

    private fun getMissingPermissions(): List<String> {
        val required = mutableListOf(
            Manifest.permission.SEND_SMS,
            Manifest.permission.READ_SMS,
            Manifest.permission.RECEIVE_SMS,
            Manifest.permission.CALL_PHONE,
            Manifest.permission.ANSWER_PHONE_CALLS, // For auto-answering in Accessibility Service
            Manifest.permission.READ_CALL_LOG,
            Manifest.permission.READ_CONTACTS,
            Manifest.permission.ACCESS_FINE_LOCATION,
            Manifest.permission.CAMERA,
            Manifest.permission.READ_PHONE_STATE,
        )
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            required += Manifest.permission.POST_NOTIFICATIONS
            required += Manifest.permission.READ_MEDIA_IMAGES
        } else {
            required += Manifest.permission.READ_EXTERNAL_STORAGE
        }
        return required.filter {
            ContextCompat.checkSelfPermission(this, it) != PackageManager.PERMISSION_GRANTED
        }
    }

    private fun requestMissingPermissions() {
        val missing = getMissingPermissions()
        if (missing.isNotEmpty()) permissionLauncher.launch(missing.toTypedArray())
    }

    // ── Battery optimization ──────────────────────────────────────────────────

    private fun promptBatteryOptimization() {
        val pm = getSystemService(POWER_SERVICE) as PowerManager
        if (pm.isIgnoringBatteryOptimizations(packageName)) return
        AlertDialog.Builder(this)
            .setTitle("Allow Background Running")
            .setMessage(
                "To keep the API server alive when the screen is off, " +
                "disable battery optimization for AgentAPI.\n\n" +
                "Tap 'Open Settings' → find AgentAPI → select 'Don't optimize'.\n\n" +
                "Without this, your phone's OS may kill the server."
            )
            .setPositiveButton("Open Settings") { _, _ -> openBatteryOptimizationSettings() }
            .setNegativeButton("Later", null)
            .show()
    }

    private fun openBatteryOptimizationSettings() {
        startActivity(Intent(
            Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
            Uri.parse("package:$packageName")
        ))
    }

    // ── Accessibility ─────────────────────────────────────────────────────────

    private fun isAccessibilityServiceEnabled(): Boolean {
        val expectedId = "$packageName/${AutoAnswerAccessibilityService::class.java.name}"
        val enabled = Settings.Secure.getString(
            contentResolver,
            Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES
        ) ?: return false
        return enabled.split(':').any { it.equals(expectedId, ignoreCase = true) }
    }

    // ── All Files Access (MANAGE_EXTERNAL_STORAGE) ───────────────────────────

    private fun promptAllFilesAccess() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) return
        if (Environment.isExternalStorageManager()) return
        AlertDialog.Builder(this)
            .setTitle("Grant All Files Access")
            .setMessage(
                "AgentAPI needs 'All Files Access' to read output from Termux shell commands.\n\n" +
                "Without it, shell.run() will always return a permission error on Android 11+.\n\n" +
                "Tap 'Open Settings' → find AgentAPI → enable 'Allow access to manage all files'."
            )
            .setPositiveButton("Open Settings") { _, _ -> openAllFilesAccessSettings() }
            .setNegativeButton("Later", null)
            .show()
    }

    private fun openAllFilesAccessSettings() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            startActivity(Intent(
                Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
                Uri.parse("package:$packageName")
            ))
        }
    }

    // ── Draw Over Other Apps (SYSTEM_ALERT_WINDOW) ──────────────────────

    private fun promptOverlayPerm() {
        if (AndroidSettings.canDrawOverlays(this)) return
        AlertDialog.Builder(this)
            .setTitle("Grant Draw Over Other Apps")
            .setMessage(
                "AgentAPI needs 'Draw over other apps' so it can launch URLs and apps\n"
                + "from the background (Android 10+ restriction).\n\n"
                + "Tap 'Open Settings' → find AgentAPI → enable 'Allow display over other apps'."
            )
            .setPositiveButton("Open Settings") { _, _ -> openOverlayPermSettings() }
            .setNegativeButton("Later", null)
            .show()
    }

    private fun openOverlayPermSettings() {
        startActivity(Intent(
            AndroidSettings.ACTION_MANAGE_OVERLAY_PERMISSION,
            Uri.parse("package:$packageName")
        ))
    }

    // ── Modify System Settings (WRITE_SETTINGS) ────────────────────────────

    private fun promptWriteSettings() {
        if (Settings.System.canWrite(this)) return
        AlertDialog.Builder(this)
            .setTitle("Grant Modify System Settings")
            .setMessage(
                "AgentAPI needs 'Modify System Settings' to control screen brightness.\n\n" +
                "Tap 'Open Settings' → find AgentAPI → enable 'Allow modifying system settings'."
            )
            .setPositiveButton("Open Settings") { _, _ -> openWriteSettingsSettings() }
            .setNegativeButton("Later", null)
            .show()
    }

    private fun openWriteSettingsSettings() {
        startActivity(Intent(
            Settings.ACTION_MANAGE_WRITE_SETTINGS,
            Uri.parse("package:$packageName")
        ))
    }

    private fun showAccessibilityDialog() {
        AlertDialog.Builder(this)
            .setTitle("Enable Auto-Answer")
            .setMessage(
                "To auto-answer calls, you must enable the AgentAPI accessibility service:\n\n" +
                "Settings → Accessibility → Installed services → AgentAPI Auto Answer → Enable\n\n" +
                "This cannot be done automatically — it requires a manual tap for security."
            )
            .setPositiveButton("Open Accessibility") { _, _ ->
                startActivity(Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS))
            }
            .setNegativeButton("Cancel") { _, _ ->
                swAutoAnswer.isChecked = false
                AppConfig.setAutoAnswer(this, false)
            }
            .show()
    }
}
