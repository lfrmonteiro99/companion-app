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
    // Last partial from the in-flight utterance. Committed to the queue
    // if the recognizer errors out or ends silence before finalising — on
    // Samsung the final onResults sometimes just never arrives.
    @Volatile private var lastPartial: String? = null

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
            putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, true)
            // Let the recognizer wait longer before calling an utterance
            // finished. Samsung's default (~500 ms complete-silence) cuts
            // off natural pauses; 2 s gives sentences room to breathe.
            putExtra(RecognizerIntent.EXTRA_SPEECH_INPUT_COMPLETE_SILENCE_LENGTH_MILLIS, 2000)
            putExtra(
                RecognizerIntent.EXTRA_SPEECH_INPUT_POSSIBLY_COMPLETE_SILENCE_LENGTH_MILLIS,
                1500,
            )
            putExtra(RecognizerIntent.EXTRA_SPEECH_INPUT_MINIMUM_LENGTH_MILLIS, 1500)
            if (Build.VERSION.SDK_INT >= 33) {
                putExtra(RecognizerIntent.EXTRA_PREFER_OFFLINE, true)
            }
        }
        lastPartial = null
        try {
            r.startListening(intent)
        } catch (t: Throwable) {
            AppLog.e(TAG, "startListening failed", t)
            scheduleRestart(RESTART_DELAY_MS)
        }
    }

    private fun scheduleRestart(delayMs: Long = RESTART_DELAY_MS) {
        if (!running) return
        main.postDelayed({ listenOnce() }, delayMs)
    }

    /** Commit whatever partial we captured so a clipped utterance isn't
     *  lost when the recognizer returns onError instead of onResults. */
    private fun commitPartial() {
        val p = lastPartial?.takeIf { it.isNotBlank() }
        if (p != null) queue.add(p)
        lastPartial = null
    }

    private fun errorName(code: Int): String = when (code) {
        SpeechRecognizer.ERROR_NETWORK_TIMEOUT -> "NETWORK_TIMEOUT"
        SpeechRecognizer.ERROR_NETWORK -> "NETWORK"
        SpeechRecognizer.ERROR_AUDIO -> "AUDIO"
        SpeechRecognizer.ERROR_SERVER -> "SERVER"
        SpeechRecognizer.ERROR_CLIENT -> "CLIENT"
        SpeechRecognizer.ERROR_SPEECH_TIMEOUT -> "SPEECH_TIMEOUT"
        SpeechRecognizer.ERROR_NO_MATCH -> "NO_MATCH"
        SpeechRecognizer.ERROR_RECOGNIZER_BUSY -> "RECOGNIZER_BUSY"
        SpeechRecognizer.ERROR_INSUFFICIENT_PERMISSIONS -> "INSUFFICIENT_PERMISSIONS"
        SpeechRecognizer.ERROR_TOO_MANY_REQUESTS -> "TOO_MANY_REQUESTS"
        SpeechRecognizer.ERROR_SERVER_DISCONNECTED -> "SERVER_DISCONNECTED"
        SpeechRecognizer.ERROR_LANGUAGE_NOT_SUPPORTED -> "LANGUAGE_NOT_SUPPORTED"
        SpeechRecognizer.ERROR_LANGUAGE_UNAVAILABLE -> "LANGUAGE_UNAVAILABLE"
        else -> "UNKNOWN_$code"
    }

    private val listener = object : RecognitionListener {
        override fun onReadyForSpeech(params: Bundle?) {
            TraceLog.micStatus("listening")
        }
        override fun onBeginningOfSpeech() {
            lastPartial = null
        }
        override fun onRmsChanged(rmsdB: Float) {}
        override fun onBufferReceived(buffer: ByteArray?) {}
        override fun onEndOfSpeech() {}
        override fun onEvent(eventType: Int, params: Bundle?) {}

        override fun onResults(results: Bundle?) {
            val list = results?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)
            val best = list?.firstOrNull()?.takeIf { it.isNotBlank() }
            if (best != null) queue.add(best)
            else commitPartial()
            lastPartial = null
            scheduleRestart()
        }

        override fun onPartialResults(partialResults: Bundle?) {
            val list = partialResults?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)
            val best = list?.firstOrNull()?.takeIf { it.isNotBlank() }
            if (best != null) lastPartial = best
        }

        override fun onError(error: Int) {
            val name = errorName(error)
            // Don't lose whatever fragment we had when the recognizer
            // bails out. Gmail drafts, short commands etc. often land
            // here on Samsung instead of the proper onResults.
            commitPartial()
            val delay: Long = when (error) {
                SpeechRecognizer.ERROR_NO_MATCH,
                SpeechRecognizer.ERROR_SPEECH_TIMEOUT,
                -> RESTART_DELAY_MS // common, silent restart
                SpeechRecognizer.ERROR_RECOGNIZER_BUSY,
                SpeechRecognizer.ERROR_TOO_MANY_REQUESTS,
                -> {
                    TraceLog.micStatus("$name — backing off 5s")
                    5_000L
                }
                else -> {
                    TraceLog.micStatus("error $name — restarting")
                    AppLog.w(TAG, "recognizer error=$name")
                    1_000L
                }
            }
            scheduleRestart(delay)
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
