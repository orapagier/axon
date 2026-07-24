package com.axon.voice.ui

import android.os.Bundle
import android.text.InputType
import android.util.TypedValue
import android.view.Gravity
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.Switch
import android.widget.TextView
import android.widget.Toast
import androidx.appcompat.app.AlertDialog
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.ContextCompat
import com.axon.voice.Prefs
import com.axon.voice.R
import com.axon.voice.api.AxonClient
import org.json.JSONObject
import kotlin.concurrent.thread

/**
 * Connection settings (server URL + master key, stored on-device) plus the
 * server-side voice settings — the same `stt.*` (Voice Input) and `tts.*`
 * (Voice Replies) rows the dashboard's Settings page edits, loaded from
 * GET /api/settings and written back per-key via PUT /api/settings/{key}.
 * The model fields offer the same catalogue picker as the web dropdowns
 * (POST /api/audio/models), with free text always allowed.
 *
 * Barge-in (interrupting a spoken reply by talking over it) is a single on/off
 * switch here — no thresholds to tune. The detector self-calibrates its echo
 * rejection ([com.axon.voice.audio.BargeDetector]); when the switch is off the
 * mic isn't watched during a reply at all, so you wait for it to finish.
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
    private val voiceRows = mutableListOf<Row>()
    private var voiceLoading = false

    /** Show base_url and model above voice/api_key/language, like a setup flow
     *  reads — the API returns rows in key order, which buries base_url. */
    private val fieldOrder = listOf("base_url", "model", "voice", "api_key", "language")

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

        val bargeSwitch = findViewById<Switch>(R.id.bargeEnabledSwitch)
        bargeSwitch.isChecked = prefs.bargeInEnabled
        bargeSwitch.setOnCheckedChangeListener { _, checked -> prefs.bargeInEnabled = checked }

        serverUrl.setText(prefs.baseUrl)
        masterKey.setText(prefs.masterKey)

        fun persist() {
            prefs.baseUrl = serverUrl.text.toString()
            prefs.masterKey = masterKey.text.toString()
        }

        findViewById<Button>(R.id.saveBtn).setOnClickListener {
            persist()
            saveVoiceSettings()
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

    /** Push edited voice rows to the server, then close. Local prefs are
     *  already persisted by the caller; with no dirty rows this just closes. */
    private fun saveVoiceSettings() {
        val dirty = voiceRows.filter { it.edit.text.toString() != it.saved }
        if (dirty.isEmpty()) {
            toastMsg(getString(R.string.saved))
            finish()
            return
        }
        thread(name = "axon-settings-save") {
            val failed = mutableListOf<String>()
            for (row in dirty) {
                val v = row.edit.text.toString()
                if (client.updateSetting(row.key, v)) row.saved = v
                else failed.add(row.key)
            }
            runOnUiThread {
                if (failed.isEmpty()) {
                    toastMsg(getString(R.string.saved))
                    finish()
                } else {
                    toastMsg(getString(R.string.voice_settings_save_failed, failed.joinToString(", ")))
                }
            }
        }
    }

    private fun dp(v: Int): Int =
        TypedValue.applyDimension(
            TypedValue.COMPLEX_UNIT_DIP, v.toFloat(), resources.displayMetrics
        ).toInt()

    private fun toastMsg(msg: String) {
        Toast.makeText(this, msg, Toast.LENGTH_SHORT).show()
    }
}
