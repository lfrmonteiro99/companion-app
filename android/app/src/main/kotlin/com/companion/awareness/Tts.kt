package com.companion.awareness

import android.content.Context
import android.speech.tts.TextToSpeech
import android.speech.tts.UtteranceProgressListener
import java.util.Locale
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Wraps android.speech.tts.TextToSpeech so AwarenessService can just
 * call Tts.speak(msg). Mirrors the role of the desktop tts.rs module.
 *
 * Initialisation is async — TextToSpeech() dispatches the engine lookup
 * to a service binding; speak() calls queued before the init callback
 * are dropped silently (acceptable for alerts, which only make sense
 * on fresh events anyway).
 *
 * Language preference: Portuguese (Portugal) first — matches the system
 * prompt's language — then falls back to the system default if the
 * engine doesn't have a pt-PT voice installed.
 */
object Tts {
    private var tts: TextToSpeech? = null
    private val ready = AtomicBoolean(false)

    fun ensureStarted(ctx: Context) {
        if (tts != null) return
        tts = TextToSpeech(ctx.applicationContext) { status ->
            val ok = status == TextToSpeech.SUCCESS
            if (!ok) {
                AppLog.w(TAG, "TTS engine init failed: $status")
                return@TextToSpeech
            }
            val lang = listOf(Locale("pt", "PT"), Locale.getDefault())
                .firstOrNull { loc ->
                    val r = tts?.isLanguageAvailable(loc) ?: TextToSpeech.LANG_MISSING_DATA
                    r >= TextToSpeech.LANG_AVAILABLE
                }
            lang?.let { tts?.language = it }
            tts?.setOnUtteranceProgressListener(NoopListener)
            ready.set(true)
        }
    }

    fun speak(msg: String) {
        if (!ready.get()) return
        if (msg.isBlank()) return
        val safe = msg.take(MAX_CHARS)
        tts?.speak(safe, TextToSpeech.QUEUE_ADD, null, utteranceId(msg))
    }

    fun shutdown() {
        ready.set(false)
        tts?.stop()
        tts?.shutdown()
        tts = null
    }

    private fun utteranceId(msg: String): String = "awareness-${msg.hashCode()}"

    private object NoopListener : UtteranceProgressListener() {
        override fun onStart(utteranceId: String?) {}
        override fun onDone(utteranceId: String?) {}
        @Deprecated("callback deprecated in API 21")
        override fun onError(utteranceId: String?) {}
        override fun onError(utteranceId: String?, errorCode: Int) {
            android.util.Log.d(TAG, "tts error id=$utteranceId code=$errorCode")
        }
    }

    private const val TAG = "Tts"
    // Matches the budget-conscious truncation on the desktop (tts.rs
    // shortens alerts to ~240 chars so they stay under a sentence).
    private const val MAX_CHARS = 240
}
