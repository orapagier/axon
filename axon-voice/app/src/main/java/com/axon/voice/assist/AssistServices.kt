package com.axon.voice.assist

import android.content.Context
import android.content.Intent
import android.os.Bundle
import android.service.voice.VoiceInteractionService
import android.service.voice.VoiceInteractionSession
import android.service.voice.VoiceInteractionSessionService
import android.speech.RecognitionService
import android.speech.SpeechRecognizer
import com.axon.voice.ui.MainActivity

/**
 * The "Siri slot": once the user picks Axon under Settings > Default apps >
 * Digital assistant app, the power-button / assist gesture shows a session,
 * which we immediately trade for the voice screen in auto-listen mode.
 */
class AxonAssistService : VoiceInteractionService()

class AxonAssistSessionService : VoiceInteractionSessionService() {
    override fun onNewSession(args: Bundle?): VoiceInteractionSession = AxonAssistSession(this)
}

class AxonAssistSession(context: Context) : VoiceInteractionSession(context) {
    override fun onShow(args: Bundle?, showFlags: Int) {
        super.onShow(args, showFlags)
        val i = Intent(context, MainActivity::class.java)
            .addFlags(
                Intent.FLAG_ACTIVITY_NEW_TASK or
                    Intent.FLAG_ACTIVITY_CLEAR_TOP or
                    Intent.FLAG_ACTIVITY_SINGLE_TOP
            )
            .putExtra(MainActivity.EXTRA_AUTO_LISTEN, true)
        context.startActivity(i)
        finish()
    }
}

/**
 * Stub: the assistant role requires a recognition service entry, but Axon does
 * speech-to-text server-side; direct SpeechRecognizer clients get an error.
 */
class AxonRecognitionService : RecognitionService() {
    override fun onStartListening(recognizerIntent: Intent?, listener: Callback?) {
        runCatching { listener?.error(SpeechRecognizer.ERROR_CLIENT) }
    }

    override fun onCancel(listener: Callback?) {}

    override fun onStopListening(listener: Callback?) {}
}
