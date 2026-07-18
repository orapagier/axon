package com.axon.voice.ui

import android.os.Bundle
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import android.widget.Toast
import androidx.appcompat.app.AppCompatActivity
import com.axon.voice.Prefs
import com.axon.voice.R
import com.axon.voice.api.AxonClient
import kotlin.concurrent.thread

class SettingsActivity : AppCompatActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_settings)

        val prefs = Prefs(this)
        val serverUrl = findViewById<EditText>(R.id.serverUrl)
        val masterKey = findViewById<EditText>(R.id.masterKey)
        val testResult = findViewById<TextView>(R.id.testResult)

        serverUrl.setText(prefs.baseUrl)
        masterKey.setText(prefs.masterKey)

        fun persist() {
            prefs.baseUrl = serverUrl.text.toString()
            prefs.masterKey = masterKey.text.toString()
        }

        findViewById<Button>(R.id.saveBtn).setOnClickListener {
            persist()
            Toast.makeText(this, "Saved", Toast.LENGTH_SHORT).show()
            finish()
        }

        findViewById<Button>(R.id.testBtn).setOnClickListener {
            persist()
            testResult.text = "Testing…"
            thread {
                val ok = AxonClient(prefs).health()
                runOnUiThread {
                    testResult.text = if (ok) "✓ Connected" else "✗ Could not reach the server"
                }
            }
        }
    }
}
