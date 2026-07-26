package com.agentapi.android

import android.Manifest
import android.accessibilityservice.AccessibilityService
import android.content.Context
import android.content.pm.PackageManager
import android.media.AudioManager
import android.os.Build
import android.telecom.TelecomManager
import android.util.Log
import android.view.KeyEvent
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityNodeInfo
import androidx.core.app.ActivityCompat

/**
 * Auto-answers incoming calls.
 *
 * HOW IT WORKS:
 * The user enables this in: Settings → Accessibility → AgentAPI Auto Answer
 * When auto_answer is toggled ON in MainActivity (or via POST /config), this
 * service answers any incoming call automatically.
 *
 * Approach (layered for compatibility across Android versions and OEM skins):
 *
 *   1. TelecomManager.acceptRingingCall() — API 28+, most reliable
 *   2. AudioManager.KEYCODE_HEADSETHOOK  — works on most devices as a fallback
 *   3. UI button click via AccessibilityNodeInfo — last resort for stubborn OEMs
 *
 * IMPORTANT: The user must enable this in Accessibility Settings for it to work.
 * This cannot be done programmatically — it requires a user tap.
 */
class AutoAnswerAccessibilityService : AccessibilityService() {

    private val TAG = "AutoAnswerA11y"

    override fun onAccessibilityEvent(event: AccessibilityEvent) {
        // Only act when auto-answer is enabled in config
        if (!AppConfig.isAutoAnswerEnabled(this)) return

        if (event.eventType != AccessibilityEvent.TYPE_WINDOW_STATE_CHANGED) return

        val packageName = event.packageName?.toString() ?: return
        // Matches the packageNames listed in accessibility_service_config.xml
        val isCallScreen = packageName.contains("incallui", ignoreCase = true)
            || packageName.contains("dialer", ignoreCase = true)

        if (!isCallScreen) return

        Log.i(TAG, "Call screen detected ($packageName) — attempting auto-answer")
        answerCall()
    }

    private fun answerCall() {
        // Method 1: TelecomManager (API 26+ with ANSWER_PHONE_CALLS permission)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            try {
                val telecomManager = getSystemService(Context.TELECOM_SERVICE) as TelecomManager
                if (ActivityCompat.checkSelfPermission(this, Manifest.permission.ANSWER_PHONE_CALLS) == PackageManager.PERMISSION_GRANTED) {
                    @Suppress("DEPRECATION")
                    telecomManager.acceptRingingCall()
                    Log.i(TAG, "Answered via TelecomManager")
                    return
                } else {
                    Log.w(TAG, "ANSWER_PHONE_CALLS permission not granted")
                }
            } catch (e: Exception) {
                Log.w(TAG, "TelecomManager failed: ${e.message}")
            }
        }

        // Method 2: Simulate headset hook key press
        try {
            val am = getSystemService(Context.AUDIO_SERVICE) as AudioManager
            am.dispatchMediaKeyEvent(KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_HEADSETHOOK))
            am.dispatchMediaKeyEvent(KeyEvent(KeyEvent.ACTION_UP, KeyEvent.KEYCODE_HEADSETHOOK))
            Log.i(TAG, "Answered via headset hook")
            return
        } catch (e: Exception) {
            Log.w(TAG, "Headset hook failed: ${e.message}")
        }

        // Method 3: Click the answer button in the UI (OEM fallback)
        tryClickAnswerButton()
    }

    /**
     * Searches the current accessibility window for a node that looks like
     * an "answer" button and clicks it. This handles OEM-customized dialers
     * that don't respond to the above methods (common on MIUI, OneUI, etc.)
     */
    private fun tryClickAnswerButton() {
        val root = rootInActiveWindow ?: return
        val answerKeywords = listOf("answer", "accept", "terima", "atender", "répondre")

        fun findAndClick(node: AccessibilityNodeInfo): Boolean {
            val desc  = node.contentDescription?.toString()?.lowercase() ?: ""
            val text  = node.text?.toString()?.lowercase() ?: ""
            val resId = node.viewIdResourceName?.lowercase() ?: ""

            val isAnswerButton = answerKeywords.any { kw ->
                desc.contains(kw) || text.contains(kw) || resId.contains(kw)
            }

            if (isAnswerButton && node.isClickable) {
                node.performAction(AccessibilityNodeInfo.ACTION_CLICK)
                Log.i(TAG, "Clicked answer button: desc=$desc text=$text id=$resId")
                return true
            }

            for (i in 0 until node.childCount) {
                val child = node.getChild(i) ?: continue
                if (findAndClick(child)) return true
            }
            return false
        }

        if (!findAndClick(root)) {
            Log.w(TAG, "Could not find answer button in UI")
        }
    }

    override fun onInterrupt() {
        Log.w(TAG, "Service interrupted")
    }

    override fun onServiceConnected() {
        super.onServiceConnected()
        Log.i(TAG, "AutoAnswer accessibility service connected")
    }
}
