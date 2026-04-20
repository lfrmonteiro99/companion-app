package com.companion.awareness

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.speech.RecognitionListener
import android.speech.RecognizerIntent
import android.speech.SpeechRecognizer
import androidx.core.content.ContextCompat
import java.util.Locale
import java.util.concurrent.ConcurrentLinkedQueue

/**
 * Passive speech-to-text using the Android `SpeechRecognizer`.
 *
 * We use the on-device recognizer when available (API 31+ with
 * `createOnDeviceSpeechRecognizer`, which requires API 33 for no-cloud
 * behaviour) and fall back to the regular one otherwise. The regular
 * recognizer may send audio through Google's servers depending on the
 * OEM — acceptable for a POC, flag this in the UI before shipping.
 *
 * Lifecycle: recognition must run on the main looper. We restart it in
 * a loop — each call listens for one utterance and returns once the
 * user pauses, then we start a fresh session. Missed audio between
 * sessions (~100ms) is the cost of this "passive" pattern on Android.
 *
 * Results are queued; [drainTranscript] returns the oldest unseen
 * transcript and the tick loop in [AwarenessService] consumes them.
 */
class AudioCapture(private val ctx: Context) {

    private val main = Handler(Looper.getMainLooper())
    private var recognizer: SpeechRecognizer? = null
    private val queue = ConcurrentLinkedQueue<String>()
    @Volatile private var running = false

    fun start() {
        val granted = ContextCompat.checkSelfPermission(
            ctx, Manifest.permission.RECORD_AUDIO,
        ) == PackageManager.PERMISSION_GRANTED
        if (!granted) {
            AppLog.w(TAG, "RECORD_AUDIO not granted; mic capture disabled")
            return
        }
        if (!SpeechRecognizer.isRecognitionAvailable(ctx)) {
            AppLog.w(TAG, "SpeechRecognizer unavailable on this device")
            return
        }

        main.post {
            val r = createRecognizer()
            recognizer = r
            r.setRecognitionListener(listener)
            running = true
            listenOnce()
        }
    }

    private fun createRecognizer(): SpeechRecognizer {
        // On-device path: API 33+. Avoids the cloud round-trip and the
        // associated privacy footprint.
        if (Build.VERSION.SDK_INT >= 33 &&
            SpeechRecognizer.isOnDeviceRecognitionAvailable(ctx)
        ) {
            return SpeechRecognizer.createOnDeviceSpeechRecognizer(ctx)
        }
        return SpeechRecognizer.createSpeechRecognizer(ctx)
    }

    private fun listenOnce() {
        val r = recognizer ?: return
        if (!running) return
        val intent = Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH).apply {
            putExtra(
                RecognizerIntent.EXTRA_LANGUAGE_MODEL,
                RecognizerIntent.LANGUAGE_MODEL_FREE_FORM,
            )
            putExtra(RecognizerIntent.EXTRA_LANGUAGE, Locale.getDefault().toLanguageTag())
            // Partial results so we don't lose long utterances if the
            // final callback arrives late.
            putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, true)
            if (Build.VERSION.SDK_INT >= 33) {
                putExtra(RecognizerIntent.EXTRA_PREFER_OFFLINE, true)
            }
        }
        try {
            r.startListening(intent)
        } catch (t: Throwable) {
            AppLog.e(TAG, "startListening failed", t)
            scheduleRestart()
        }
    }

    private fun scheduleRestart() {
        if (!running) return
        main.postDelayed({ listenOnce() }, RESTART_DELAY_MS)
    }

    private val listener = object : RecognitionListener {
        override fun onReadyForSpeech(params: Bundle?) {}
        override fun onBeginningOfSpeech() {}
        override fun onRmsChanged(rmsdB: Float) {}
        override fun onBufferReceived(buffer: ByteArray?) {}
        override fun onEndOfSpeech() {}
        override fun onEvent(eventType: Int, params: Bundle?) {}

        override fun onResults(results: Bundle?) {
            val list = results?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)
            val best = list?.firstOrNull()?.takeIf { it.isNotBlank() }
            if (best != null) queue.add(best)
            scheduleRestart()
        }

        override fun onPartialResults(partialResults: Bundle?) {
            // Intentionally ignore — we only keep finalised utterances
            // to avoid duplicating text in the tick payload.
        }

        override fun onError(error: Int) {
            when (error) {
                SpeechRecognizer.ERROR_NO_MATCH,
                SpeechRecognizer.ERROR_SPEECH_TIMEOUT,
                -> { /* common; just restart */ }
                else -> android.util.Log.d(TAG, "recognizer error=$error")
            }
            scheduleRestart()
        }
    }

    /**
     * Drains all pending utterances joined with "; " — matches the
     * shape of `mic_text_recent` on the desktop, where multiple whisper
     * chunks arriving in a tick are merged into a single string.
     * Returns null when nothing new since the last call.
     */
    fun drainTranscript(): String? {
        if (queue.isEmpty()) return null
        val parts = mutableListOf<String>()
        while (true) {
            val next = queue.poll() ?: break
            parts.add(next)
        }
        return parts.joinToString("; ").takeIf { it.isNotBlank() }
    }

    fun stop() {
        running = false
        main.post {
            recognizer?.stopListening()
            recognizer?.destroy()
            recognizer = null
        }
    }

    companion object {
        private const val TAG = "AudioCapture"
        private const val RESTART_DELAY_MS = 250L
    }
}
