package com.axon.voice.ui

import android.Manifest
import android.content.pm.PackageManager
import android.os.Bundle
import android.widget.Button
import android.widget.EditText
import android.widget.ProgressBar
import android.widget.TextView
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.ContextCompat
import com.axon.voice.Prefs
import com.axon.voice.R
import com.axon.voice.audio.SilenceWatcher
import com.axon.voice.audio.WavRecorder
import com.axon.voice.wake.WakeDetector
import com.axon.voice.wake.WakeModelBuilder
import com.axon.voice.wake.WakeWordService
import java.io.File
import kotlin.concurrent.thread

/**
 * On-device wake-word enrollment: record a handful of takes of a phrase (any
 * phrase, not just "Hey Axon") and build a personal .rpw model locally via
 * [WakeModelBuilder] — the same rustpotter builder `rustpotter-cli build`
 * uses, just run on-device per user instead of once from the developer's own
 * voice. Launched from [ChatActivity]'s wake toggle the first time wake-word
 * is turned on with nothing enrolled, or from [SettingsActivity] to re-train;
 * finishes with RESULT_OK once a model is saved.
 */
class EnrollWakeWordActivity : AppCompatActivity() {

    companion object {
        private const val TAKE_COUNT = 5
        private const val MIN_TAKES = 3
    }

    private lateinit var prefs: Prefs
    private lateinit var phraseInput: EditText
    private lateinit var statusText: TextView
    private lateinit var levelBar: ProgressBar
    private lateinit var recordBtn: Button
    private lateinit var rerecordBtn: Button
    private lateinit var buildBtn: Button
    private lateinit var resultText: TextView

    private val takes = mutableListOf<ByteArray>()
    private var recorder: WavRecorder? = null
    private var watcher: SilenceWatcher? = null
    private var recording = false
    private var built = false

    private val permLauncher =
        registerForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { grants ->
            if (grants[Manifest.permission.RECORD_AUDIO] == true) startTake()
        }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_enroll_wake)

        prefs = Prefs(this)
        phraseInput = findViewById(R.id.wakePhraseInput)
        statusText = findViewById(R.id.enrollStatus)
        levelBar = findViewById(R.id.enrollLevel)
        recordBtn = findViewById(R.id.enrollRecordBtn)
        rerecordBtn = findViewById(R.id.enrollRerecordBtn)
        buildBtn = findViewById(R.id.enrollBuildBtn)
        resultText = findViewById(R.id.enrollResult)

        phraseInput.setText(prefs.wakePhrase.ifEmpty { "Hey Axon" })

        if (!WakeModelBuilder.available) {
            statusText.text = getString(R.string.enroll_engine_missing)
            phraseInput.isEnabled = false
            return
        }

        recordBtn.setOnClickListener {
            if (!hasMicPermission()) {
                permLauncher.launch(arrayOf(Manifest.permission.RECORD_AUDIO))
            } else {
                startTake()
            }
        }
        rerecordBtn.setOnClickListener { redoLastTake() }
        buildBtn.setOnClickListener { buildModel() }

        renderTakes()
    }

    private fun hasMicPermission(): Boolean =
        ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO) ==
            PackageManager.PERMISSION_GRANTED

    // ── Capture ──────────────────────────────────────────────────────────────

    private fun startTake() {
        if (recording || built || takes.size >= TAKE_COUNT) return
        recording = true
        recordBtn.isEnabled = false
        rerecordBtn.isEnabled = false
        buildBtn.isEnabled = false
        statusText.text =
            getString(R.string.enroll_recording_status, takes.size + 1, TAKE_COUNT, phrase())

        val w = SilenceWatcher()
        watcher = w
        val r = WavRecorder(cleanSignal = true)
        recorder = r
        val serviceWasListening = WakeWordService.running
        WakeWordService.micHold = true
        thread(name = "axon-enroll-rec") {
            // Give the wake service a beat to release the shared microphone.
            if (serviceWasListening) Thread.sleep(300)
            try {
                r.start { rms ->
                    runOnUiThread { levelBar.progress = (rms * 400).toInt().coerceIn(0, 100) }
                    if (w.tick(rms)) runOnUiThread { stopTake() }
                }
            } catch (e: Exception) {
                WakeWordService.micHold = false
                runOnUiThread {
                    recording = false
                    statusText.text = e.message ?: "microphone unavailable"
                    renderTakes()
                }
            }
        }
    }

    private fun stopTake() {
        if (!recording) return
        recording = false
        val r = recorder
        val w = watcher
        recorder = null
        watcher = null
        levelBar.progress = 0
        thread(name = "axon-enroll-stop") {
            val wav = r?.stop()
            WakeWordService.micHold = false
            runOnUiThread {
                if (wav != null && w?.hadSpeech == true) {
                    takes.add(wav)
                } else {
                    statusText.text = getString(R.string.enroll_no_speech)
                }
                renderTakes()
            }
        }
    }

    private fun redoLastTake() {
        if (takes.isNotEmpty()) takes.removeAt(takes.size - 1)
        renderTakes()
    }

    private fun renderTakes() {
        if (!recording) {
            statusText.text = getString(R.string.enroll_takes_status, takes.size, TAKE_COUNT)
        }
        recordBtn.isEnabled = !recording && !built && takes.size < TAKE_COUNT
        recordBtn.text =
            getString(if (takes.isEmpty()) R.string.enroll_record else R.string.enroll_record_next)
        rerecordBtn.isEnabled = !recording && !built && takes.isNotEmpty()
        buildBtn.isEnabled = !recording && !built && takes.size >= MIN_TAKES
    }

    private fun phrase(): String = phraseInput.text.toString().trim().ifEmpty { "Hey Axon" }

    // ── Build ────────────────────────────────────────────────────────────────

    private fun buildModel() {
        recordBtn.isEnabled = false
        rerecordBtn.isEnabled = false
        buildBtn.isEnabled = false
        resultText.text = "Building…"
        val phraseText = phrase()
        val samples = takes.toList()
        thread(name = "axon-enroll-build") {
            val model = WakeModelBuilder.build(phraseText, samples)
            if (model == null) {
                runOnUiThread {
                    resultText.text = getString(R.string.enroll_build_failed)
                    renderTakes()
                }
                return@thread
            }
            val selfTestOk = selfTest(model, samples.last())
            val file = File(filesDir, "wake_model.rpw")
            file.writeBytes(model)
            prefs.wakeModelPath = file.absolutePath
            prefs.wakePhrase = phraseText
            runOnUiThread {
                built = true
                resultText.text = getString(
                    if (selfTestOk) R.string.enroll_build_ok else R.string.enroll_build_ok_no_selftest
                )
                buildBtn.text = getString(R.string.enroll_done)
                buildBtn.isEnabled = true
                buildBtn.setOnClickListener { finishEnrolled() }
            }
        }
    }

    /** Sanity check: does the model we just built actually fire on one of its
     *  own takes? Advisory only — a miss here doesn't block saving, since a
     *  false negative against a single clip is common even for a good model. */
    private fun selfTest(model: ByteArray, sampleWav: ByteArray): Boolean {
        val detector = try {
            WakeDetector(model, name = "enroll-selftest")
        } catch (_: Exception) {
            return false
        }
        return try {
            val pcm = WavRecorder.pcmFromWav(sampleWav)
            val frame = detector.samplesPerFrame
            var i = 0
            var fired = false
            while (i + frame <= pcm.size) {
                if (detector.process(pcm.copyOfRange(i, i + frame)) >= 0f) {
                    fired = true
                    break
                }
                i += frame
            }
            fired
        } finally {
            detector.close()
        }
    }

    private fun finishEnrolled() {
        setResult(RESULT_OK)
        finish()
    }

    override fun onDestroy() {
        if (recording) {
            recording = false
            WakeWordService.micHold = false
            recorder?.let { r -> thread { r.stop() } }
        }
        super.onDestroy()
    }
}
