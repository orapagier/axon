package com.agentapi.android

import com.axon.voice.R
import android.app.KeyguardManager
import android.app.NotificationManager
import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.media.Ringtone
import android.media.RingtoneManager
import android.os.Build
import android.os.Bundle
import android.os.VibrationEffect
import android.os.Vibrator
import android.os.VibratorManager
import android.view.WindowManager
import android.widget.Button
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity

/**
 * Full-screen alarm activity that fires when the AlarmManager alarm goes off.
 *
 * It is launched via a fullScreenIntent from AlarmRingerReceiver, which means
 * Android grants it a special exemption from background activity start restrictions —
 * it will appear over the lock screen regardless of what app is in the foreground.
 *
 * Responsibilities:
 *   1. Turn the screen on and show over the lock screen
 *   2. Play the alarm ringtone at alarm volume
 *   3. Vibrate in an alarm pattern
 *   4. Provide a Dismiss button that cleanly stops everything
 *   5. Auto-dismiss after 5 minutes so it never hangs forever
 */
class AlarmDismissActivity : AppCompatActivity() {

    companion object {
        const val EXTRA_LABEL = "label"
        private const val AUTO_DISMISS_MS = 5 * 60 * 1000L  // 5 minutes
    }

    private var ringtone: Ringtone? = null
    private var vibrator: Vibrator? = null
    private var audioFocusRequest: AudioFocusRequest? = null
    private val autoDismissRunnable = Runnable { dismissAlarm() }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        // ── Step 1: Wake screen and show over lock screen ─────────────────────
        //
        // On API 27+ use the dedicated KeyguardManager + window flags API.
        // Below API 27 we fall back to the legacy window flags.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O_MR1) {
            setShowWhenLocked(true)
            setTurnScreenOn(true)
            val km = getSystemService(Context.KEYGUARD_SERVICE) as KeyguardManager
            km.requestDismissKeyguard(this, null)
        } else {
            @Suppress("DEPRECATION")
            window.addFlags(
                WindowManager.LayoutParams.FLAG_SHOW_WHEN_LOCKED or
                WindowManager.LayoutParams.FLAG_TURN_SCREEN_ON   or
                WindowManager.LayoutParams.FLAG_DISMISS_KEYGUARD or
                WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON
            )
        }
        // Keep screen on for the duration of the alarm
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)

        setContentView(R.layout.activity_alarm_dismiss)

        val label = intent.getStringExtra(EXTRA_LABEL) ?: "Alarm"
        findViewById<TextView>(R.id.tv_alarm_label).text = label

        findViewById<Button>(R.id.btn_dismiss).setOnClickListener { dismissAlarm() }

        // ── Step 2: Request audio focus so music/podcasts duck ────────────────
        val am = getSystemService(Context.AUDIO_SERVICE) as AudioManager
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val req = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT)
                .setAudioAttributes(
                    AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_ALARM)
                        .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                        .build()
                )
                .build()
            am.requestAudioFocus(req)
            audioFocusRequest = req
        } else {
            @Suppress("DEPRECATION")
            am.requestAudioFocus(null, AudioManager.STREAM_ALARM, AudioManager.AUDIOFOCUS_GAIN_TRANSIENT)
        }

        // ── Step 3: Play alarm ringtone ───────────────────────────────────────
        val alarmUri = RingtoneManager.getDefaultUri(RingtoneManager.TYPE_ALARM)
            ?: RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE)
        ringtone = RingtoneManager.getRingtone(this, alarmUri)?.also { rt ->
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                rt.isLooping = true
            }
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                rt.audioAttributes = AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_ALARM)
                    .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                    .build()
            }
            rt.play()
        }

        // ── Step 4: Vibrate in alarm pattern ──────────────────────────────────
        val pattern = longArrayOf(0, 500, 500, 500, 500, 500)
        vibrator = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            (getSystemService(Context.VIBRATOR_MANAGER_SERVICE) as VibratorManager).defaultVibrator
        } else {
            @Suppress("DEPRECATION")
            getSystemService(Context.VIBRATOR_SERVICE) as Vibrator
        }
        vibrator?.let { v ->
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                v.vibrate(VibrationEffect.createWaveform(pattern, 0))
            } else {
                @Suppress("DEPRECATION")
                v.vibrate(pattern, 0)
            }
        }

        // ── Step 5: Auto-dismiss after 5 minutes ──────────────────────────────
        window.decorView.postDelayed(autoDismissRunnable, AUTO_DISMISS_MS)
    }

    private fun dismissAlarm() {
        window.decorView.removeCallbacks(autoDismissRunnable)
        ringtone?.stop()
        vibrator?.cancel()
        audioFocusRequest?.let { req ->
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                (getSystemService(Context.AUDIO_SERVICE) as AudioManager).abandonAudioFocusRequest(req)
            }
        }
        // Cancel the notification so it disappears from the shade and
        // can't be tapped again to re-trigger the alarm sound
        (getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager)
            .cancel(AlarmRingerReceiver.NOTIF_ID)
        finish()
    }

    // If user presses back, treat it as dismiss
    @Deprecated("Deprecated in Java")
    override fun onBackPressed() {
        dismissAlarm()
    }

    override fun onDestroy() {
        // Safety net: stop everything if activity is killed externally
        ringtone?.stop()
        vibrator?.cancel()
        super.onDestroy()
    }
}
