package com.axon.androidcompanion.ui

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Environment
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.provider.Settings
import android.text.InputType
import android.util.TypedValue
import android.view.Gravity
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.SeekBar
import android.widget.Switch
import android.widget.TextView
import android.widget.Toast
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AlertDialog
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.ContextCompat
import com.axon.androidcompanion.Prefs
import com.axon.androidcompanion.R
import com.axon.androidcompanion.api.AxonClient
import com.axon.androidcompanion.device.ApiServer
import com.axon.androidcompanion.device.ApiService
import com.axon.androidcompanion.device.AppConfig
import com.axon.androidcompanion.device.AutoAnswerAccessibilityService
import com.axon.androidcompanion.device.CloudflaredManager
import com.google.android.material.switchmaterial.SwitchMaterial
import org.json.JSONObject
import kotlin.concurrent.thread

/**
 * The app's one settings screen:
 *  - Connection (server URL + master key, stored on-device) — its own Save + Test
 *  - Server-side voice settings — the same `stt.*` (Voice Input) and `tts.*`
 *    (Voice Replies) rows the dashboard's Settings page edits, loaded from
 *    GET /api/settings and written back per-key via PUT /api/settings/{key}.
 *    Each category gets its own Save button, mirroring the dashboard's
 *    per-category save. The model fields offer the same catalogue picker as
 *    the web dropdowns (POST /api/audio/models), with free text always allowed.
 *  - Follow-up window / barge-in / wake word tuning
 *  - Device control: the local HTTP API + Cloudflare tunnel exposing SMS/calls/
 *    files/shell/etc to axon-agent. Used to be its own DeviceApiActivity screen;
 *    folded in here so there is only one settings page. ChatActivity's landing-page
 *    switch starts/stops the same ApiService for a quick on/off; this section is
 *    the detailed status/config view.
 *
 * No category here auto-closes the page on save — each save button is
 * independent, so the user can edit several sections before leaving via the
 * system back gesture.
 */
class SettingsActivity : AppCompatActivity() {

    /** One editable server setting row and its last-known saved value. */
    private class Row(
        val key: String,
        val category: String,
        var saved: String,
        val edit: EditText,
    )

    private lateinit var prefs: Prefs
    private lateinit var client: AxonClient
    private lateinit var voiceStatus: TextView
    private lateinit var voiceContainer: LinearLayout
    private lateinit var wakePhraseStatus: TextView
    private val voiceRows = mutableListOf<Row>()
    private var voiceLoading = false

    /** Show base_url and model above voice/api_key/language, like a setup flow
     *  reads — the API returns rows in key order, which buries base_url. */
    private val fieldOrder = listOf("base_url", "model", "voice", "api_key", "language")

    // ── Device control views ─────────────────────────────────────────────────
    private val handler       = Handler(Looper.getMainLooper())
    private val statusUpdater = object : Runnable {
        override fun run() { refreshDeviceStatus(); handler.postDelayed(this, 2000) }
    }

    private lateinit var tvStatus       : TextView
    private lateinit var tvUrl          : TextView
    private lateinit var tvToken        : TextView
    private lateinit var tvTermux       : TextView
    private lateinit var tvPermissions  : TextView
    private lateinit var tvTunnelStatus : TextView
    private lateinit var etWebhook      : EditText
    private lateinit var etCloudflaredToken      : EditText
    private lateinit var btnSaveCloudflaredToken : Button
    private lateinit var btnToggle      : Button
    private lateinit var btnSaveWebhook : Button
    private lateinit var btnPermissions : Button
    private lateinit var btnBatteryOpt  : Button
    private lateinit var btnAllFiles    : Button
    private lateinit var btnWriteSettings   : Button
    private lateinit var btnOverlayPerm     : Button
    private lateinit var swAutoAnswer   : SwitchMaterial
    private lateinit var swPushSms      : SwitchMaterial
    private lateinit var swPushCalls    : SwitchMaterial
    private lateinit var swPushLocation : SwitchMaterial
    private lateinit var swPushBattery  : SwitchMaterial

    private val permissionLauncher = registerForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions()
    ) { refreshDeviceStatus() }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_settings)

        prefs = Prefs(this)
        client = AxonClient(prefs)
        val serverUrl = findViewById<EditText>(R.id.serverUrl)
        val masterKey = findViewById<EditText>(R.id.masterKey)
        val testResult = findViewById<TextView>(R.id.testResult)
        voiceStatus = findViewById(R.id.voiceStatus)
        voiceContainer = findViewById(R.id.voiceContainer)

        buildFollowupTuner()
        buildBargeTuner()
        wakePhraseStatus = findViewById(R.id.wakePhraseStatus)
        findViewById<Button>(R.id.wakeRetrainBtn).setOnClickListener {
            startActivity(Intent(this, EnrollWakeWordActivity::class.java))
        }

        bindDeviceControlViews()
        loadDeviceControlConfig()
        requestMissingPermissions()
        promptBatteryOptimization()
        promptAllFilesAccess()
        promptWriteSettings()
        promptOverlayPerm()

        serverUrl.setText(prefs.baseUrl)
        masterKey.setText(prefs.masterKey)

        fun persist() {
            prefs.baseUrl = serverUrl.text.toString()
            prefs.masterKey = masterKey.text.toString()
        }

        findViewById<Button>(R.id.saveBtn).setOnClickListener {
            persist()
            toastMsg(getString(R.string.saved))
        }

        findViewById<Button>(R.id.testBtn).setOnClickListener {
            persist()
            testResult.text = "Testing…"
            thread {
                val ok = client.health()
                runOnUiThread {
                    testResult.text = if (ok) "✓ Connected" else "✗ Could not reach the server"
                    // A fresh URL/key that just proved good — (re)load the
                    // voice rows it serves.
                    if (ok && voiceRows.isEmpty()) loadVoiceSettings()
                }
            }
        }

        if (prefs.configured) loadVoiceSettings()
        else voiceStatus.text = getString(R.string.voice_settings_unavailable)
        renderWakeStatus()
    }

    override fun onResume() {
        super.onResume()
        // Reflects a re-enrollment done via wakeRetrainBtn while this activity
        // was paused underneath EnrollWakeWordActivity.
        renderWakeStatus()
        refreshDeviceStatus()
        handler.post(statusUpdater)
    }

    override fun onPause() {
        super.onPause()
        handler.removeCallbacks(statusUpdater)
    }

    private fun renderWakeStatus() {
        wakePhraseStatus.text = if (prefs.wakeEnrolled) {
            getString(R.string.settings_wake_enrolled_fmt, prefs.wakePhrase)
        } else {
            getString(R.string.settings_wake_not_enrolled)
        }
    }

    // ── Server-side voice settings ──────────────────────────────────────────

    private fun loadVoiceSettings() {
        if (voiceLoading) return
        voiceLoading = true
        voiceStatus.text = getString(R.string.voice_settings_loading)
        thread(name = "axon-settings-load") {
            val rows = runCatching { client.settings() }.getOrNull()
            runOnUiThread {
                voiceLoading = false
                if (rows == null) {
                    voiceStatus.text = getString(R.string.voice_settings_unavailable)
                    return@runOnUiThread
                }
                val voice = (0 until rows.length())
                    .mapNotNull { rows.optJSONObject(it) }
                    .filter { it.optString("category") in setOf("stt", "tts") }
                voiceStatus.text =
                    if (voice.isEmpty()) getString(R.string.voice_settings_unavailable) else ""
                buildVoiceUi(voice)
            }
        }
    }

    private fun buildVoiceUi(settings: List<JSONObject>) {
        voiceContainer.removeAllViews()
        voiceRows.clear()
        for ((category, title) in listOf(
            "stt" to getString(R.string.voice_input_title),
            "tts" to getString(R.string.voice_replies_title),
        )) {
            val group = settings
                .filter { it.optString("category") == category }
                .sortedBy { row ->
                    val suffix = row.optString("key").substringAfter('.')
                    fieldOrder.indexOf(suffix).let { if (it < 0) fieldOrder.size else it }
                }
            if (group.isEmpty()) continue
            voiceContainer.addView(sectionHeader(title))
            for (row in group) addSettingRow(row)
            voiceContainer.addView(categorySaveButton(category))
        }
    }

    private fun categorySaveButton(category: String): Button = Button(this).apply {
        text = getString(R.string.save)
        setPadding(0, dp(10), 0, 0)
        setOnClickListener { saveCategory(category) }
    }

    /** Push edited rows of one category (stt or tts) to the server — the same
     *  per-category Save the dashboard's Settings page uses, so editing Voice
     *  Input doesn't require also having a clean draft in Voice Replies. */
    private fun saveCategory(category: String) {
        val dirty = voiceRows.filter { it.category == category && it.edit.text.toString() != it.saved }
        if (dirty.isEmpty()) {
            toastMsg(getString(R.string.saved))
            return
        }
        thread(name = "axon-settings-save-$category") {
            val failed = mutableListOf<String>()
            for (row in dirty) {
                val v = row.edit.text.toString()
                if (client.updateSetting(row.key, v)) row.saved = v
                else failed.add(row.key)
            }
            runOnUiThread {
                if (failed.isEmpty()) toastMsg(getString(R.string.saved))
                else toastMsg(getString(R.string.voice_settings_save_failed, failed.joinToString(", ")))
            }
        }
    }

    private fun addSettingRow(setting: JSONObject) {
        val key = setting.optString("key")
        val category = setting.optString("category")
        val value = setting.optString("value")

        voiceContainer.addView(TextView(this).apply {
            text = key
            setTextColor(ContextCompat.getColor(context, R.color.text))
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 14f)
            setPadding(0, dp(14), 0, 0)
        })

        val edit = EditText(this).apply {
            setText(value)
            isSingleLine = true
            inputType = if (isSecret(setting)) {
                InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD
            } else {
                InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
            }
            setTextColor(ContextCompat.getColor(context, R.color.text))
            setHintTextColor(ContextCompat.getColor(context, R.color.text_dim))
            importantForAutofill = android.view.View.IMPORTANT_FOR_AUTOFILL_NO
        }
        val editParams = LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f)

        if (key.endsWith(".model")) {
            // Same affordance as the web dropdown: a picker listing what the
            // current base_url draft exposes; typing any ID still works.
            voiceContainer.addView(LinearLayout(this).apply {
                orientation = LinearLayout.HORIZONTAL
                gravity = Gravity.CENTER_VERTICAL
                addView(edit, editParams)
                addView(Button(context).apply {
                    text = "▾"
                    minWidth = dp(48)
                    minimumWidth = dp(48)
                    setOnClickListener { pickModel(category, edit) }
                })
            })
        } else {
            voiceContainer.addView(edit, LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT
            ))
        }

        val description = setting.optString("description")
        if (description.isNotBlank()) {
            voiceContainer.addView(TextView(this).apply {
                text = description
                setTextColor(ContextCompat.getColor(context, R.color.text_dim))
                setTextSize(TypedValue.COMPLEX_UNIT_SP, 12f)
            })
        }

        voiceRows.add(Row(key, category, value, edit))
    }

    private fun sectionHeader(title: String): TextView = TextView(this).apply {
        text = title
        setTextColor(ContextCompat.getColor(context, R.color.accent))
        setTextSize(TypedValue.COMPLEX_UNIT_SP, 16f)
        setTypeface(typeface, android.graphics.Typeface.BOLD)
        setPadding(0, dp(20), 0, 0)
    }

    private fun isSecret(setting: JSONObject): Boolean {
        if (setting.optString("value_type") != "string") return false
        val k = setting.optString("key").lowercase()
        return "key" in k || "token" in k || "password" in k
    }

    /** Draft value of a sibling row (e.g. tts.base_url while picking tts.model). */
    private fun draft(key: String): String =
        voiceRows.find { it.key == key }?.edit?.text?.toString()?.trim() ?: ""

    private fun pickModel(kind: String, target: EditText) {
        val base = draft("$kind.base_url")
        if (base.isEmpty()) {
            toastMsg(getString(R.string.voice_models_need_base_url))
            return
        }
        toastMsg(getString(R.string.voice_models_loading))
        thread(name = "axon-models") {
            val models = client.audioModels(kind, base, draft("$kind.api_key"))
            runOnUiThread {
                if (isFinishing || isDestroyed) return@runOnUiThread
                if (models.isEmpty()) {
                    toastMsg(getString(R.string.voice_models_none))
                    return@runOnUiThread
                }
                AlertDialog.Builder(this)
                    .setTitle("$kind.model")
                    .setItems(models.toTypedArray()) { _, which ->
                        target.setText(models[which])
                    }
                    .show()
            }
        }
    }

    // ── Follow-up window slider ─────────────────────────────────────────────
    // Persist on change (read fresh per reply, no save button needed) — how
    // long to keep listening for the user's next turn after a spoken reply.
    private fun buildFollowupTuner() {
        val c = findViewById<LinearLayout>(R.id.followupTuneContainer)
        // Follow-up window (ticks): 30..150 → 3..15s.
        addSlider(c, R.string.followup_tune_label, 30, 150, prefs.followupWindowTicks,
            { p -> "${p / 10}s" }) { p -> prefs.followupWindowTicks = p }
    }

    // ── Barge-in toggle + trigger loudness ──────────────────────────────────
    // Both persist on change (read fresh per reply). HEADSET ONLY — with the
    // built-in speaker the reply plays to completion. The trigger loudness is
    // the broadband RMS the mic must exceed to interrupt: tune it against the
    // live `mic=` value the notification shows while a reply plays — raise it
    // until distant/quiet voices no longer trip it but you do.
    private fun buildBargeTuner() {
        val c = findViewById<LinearLayout>(R.id.bargeTuneContainer)
        @Suppress("UseSwitchCompatOrMaterialCode")
        c.addView(Switch(this).apply {
            text = getString(R.string.barge_toggle)
            setTextColor(ContextCompat.getColor(context, R.color.text))
            isChecked = prefs.bargeInEnabled
            setPadding(0, dp(8), 0, dp(4))
            setOnCheckedChangeListener { _, v -> prefs.bargeInEnabled = v }
        })
        // Trigger loudness stored as RMS×10000 (100..3000 → 0.0100..0.3000).
        addSlider(c, R.string.barge_threshold_label, 100, 3000, prefs.bargeOnsetLevel,
            { p -> String.format("%.4f", p / 10000.0) }) { p -> prefs.bargeOnsetLevel = p }
    }

    private fun addSlider(
        parent: LinearLayout,
        labelRes: Int,
        min: Int,
        max: Int,
        current: Int,
        format: (Int) -> String,
        onChange: (Int) -> Unit,
    ) {
        val label = TextView(this).apply {
            setTextColor(ContextCompat.getColor(context, R.color.text))
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 13f)
            setPadding(0, dp(14), 0, 0)
        }
        fun render(p: Int) {
            label.text = "${getString(labelRes)}:  ${format(p)}"
        }
        val start = current.coerceIn(min, max)
        render(start)
        parent.addView(label)
        parent.addView(SeekBar(this).apply {
            this.max = max
            this.min = min
            progress = start
            setOnSeekBarChangeListener(object : SeekBar.OnSeekBarChangeListener {
                override fun onProgressChanged(sb: SeekBar, p: Int, fromUser: Boolean) {
                    val v = p.coerceIn(min, max)
                    render(v)
                    onChange(v)
                }

                override fun onStartTrackingTouch(sb: SeekBar) {}
                override fun onStopTrackingTouch(sb: SeekBar) {}
            })
        })
    }

    private fun dp(v: Int): Int =
        TypedValue.applyDimension(
            TypedValue.COMPLEX_UNIT_DIP, v.toFloat(), resources.displayMetrics
        ).toInt()

    private fun toastMsg(msg: String) {
        Toast.makeText(this, msg, Toast.LENGTH_SHORT).show()
    }

    // ══════════════════════════════════════════════════════════════════════
    // Device control — local HTTP API + Cloudflare tunnel (formerly its own
    // DeviceApiActivity screen). ChatActivity's landing-page switch starts/stops
    // the same ApiService for a quick on/off; this section is the detailed view.
    // ══════════════════════════════════════════════════════════════════════

    private fun bindDeviceControlViews() {
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
            toastMsg(getString(R.string.webhook_saved))
        }

        btnSaveCloudflaredToken.setOnClickListener {
            val tok = etCloudflaredToken.text.toString().trim()
            if (tok.isNotEmpty()) {
                AppConfig.setCloudflaredToken(this, tok)
                etCloudflaredToken.setText("")
                CloudflaredManager.start(this)
                toastMsg(getString(R.string.tunnel_token_saved))
                handler.postDelayed({ refreshDeviceStatus() }, 1500)
            } else {
                toastMsg(getString(R.string.tunnel_token_empty))
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
            toastMsg(getString(R.string.token_copied))
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

    private fun loadDeviceControlConfig() {
        etWebhook.setText(AppConfig.getWebhookUrl(this))
        swAutoAnswer.isChecked   = AppConfig.isAutoAnswerEnabled(this)
        swPushSms.isChecked      = AppConfig.isPushSmsEnabled(this)
        swPushCalls.isChecked    = AppConfig.isPushCallsEnabled(this)
        swPushLocation.isChecked = AppConfig.isPushLocationEnabled(this)
        swPushBattery.isChecked  = AppConfig.isPushBatteryEnabled(this)
    }

    private fun refreshDeviceStatus() {
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

        btnToggle.text = if (running) getString(R.string.device_stop_server) else getString(R.string.device_start_server)

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
        btnOverlayPerm.text = if (Settings.canDrawOverlays(this))
            "Draw Over Other Apps: Granted ✓"
        else
            "Grant Draw Over Other Apps ⚠  (required for launch.url/app)"
    }

    private fun toggleServer() {
        AppConfig.setDeviceControlEnabled(this, !ApiService.isRunning)
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
        handler.postDelayed({ refreshDeviceStatus() }, 800)
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
                "disable battery optimization for AndroidCompanion.\n\n" +
                "Tap 'Open Settings' → find AndroidCompanion → select 'Don't optimize'.\n\n" +
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
                "AndroidCompanion needs 'All Files Access' to read output from Termux shell commands.\n\n" +
                "Without it, shell.run() will always return a permission error on Android 11+.\n\n" +
                "Tap 'Open Settings' → find AndroidCompanion → enable 'Allow access to manage all files'."
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
        if (Settings.canDrawOverlays(this)) return
        AlertDialog.Builder(this)
            .setTitle("Grant Draw Over Other Apps")
            .setMessage(
                "AndroidCompanion needs 'Draw over other apps' so it can launch URLs and apps\n"
                + "from the background (Android 10+ restriction).\n\n"
                + "Tap 'Open Settings' → find AndroidCompanion → enable 'Allow display over other apps'."
            )
            .setPositiveButton("Open Settings") { _, _ -> openOverlayPermSettings() }
            .setNegativeButton("Later", null)
            .show()
    }

    private fun openOverlayPermSettings() {
        startActivity(Intent(
            Settings.ACTION_MANAGE_OVERLAY_PERMISSION,
            Uri.parse("package:$packageName")
        ))
    }

    // ── Modify System Settings (WRITE_SETTINGS) ────────────────────────────

    private fun promptWriteSettings() {
        if (Settings.System.canWrite(this)) return
        AlertDialog.Builder(this)
            .setTitle("Grant Modify System Settings")
            .setMessage(
                "AndroidCompanion needs 'Modify System Settings' to control screen brightness.\n\n" +
                "Tap 'Open Settings' → find AndroidCompanion → enable 'Allow modifying system settings'."
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
                "To auto-answer calls, you must enable the AndroidCompanion accessibility service:\n\n" +
                "Settings → Accessibility → Installed services → AndroidCompanion Auto Answer → Enable\n\n" +
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
