package com.companion.awareness

import android.content.Context
import android.os.SystemClock
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

    // Self-narration guard state. `speaking` is true between a TTS
    // utterance's onStart and onDone; `speakingSinceMs` bounds it so a
    // dropped terminal callback can't wedge it true forever; `lastSpokeEndMs`
    // marks when the last utterance finished so we keep muting the mic for a
    // short tail. Written from the TTS engine's binder-thread callbacks, read
    // from the recognizer's main-looper thread — @Volatile is sufficient
    // (single independent writes, no compound updates).
    @Volatile private var speaking = false
    @Volatile private var speakingSinceMs = 0L
    @Volatile private var lastSpokeEndMs = 0L

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
            tts?.setOnUtteranceProgressListener(object : UtteranceProgressListener() {
                override fun onStart(utteranceId: String?) {
                    speaking = true
                    speakingSinceMs = SystemClock.elapsedRealtime()
                }
                override fun onDone(utteranceId: String?) {
                    speaking = false
                    lastSpokeEndMs = SystemClock.elapsedRealtime()
                }
                @Deprecated("callback deprecated in API 21")
                override fun onError(utteranceId: String?) {
                    speaking = false
                    lastSpokeEndMs = SystemClock.elapsedRealtime()
                }
                override fun onError(utteranceId: String?, errorCode: Int) {
                    speaking = false
                    lastSpokeEndMs = SystemClock.elapsedRealtime()
                    android.util.Log.d(TAG, "tts error id=$utteranceId code=$errorCode")
                }
            })
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
        speaking = false
        tts?.stop()
        tts?.shutdown()
        tts = null
    }

    /**
     * True while our own TTS alert is playing, plus a tail margin after it
     * ends. AudioCapture consults this to drop transcripts that finalise
     * inside the window, so the recognizer doesn't pick up our own spoken
     * alert and re-trigger voice_activity/emotional (a self-narration loop).
     * When TTS is disabled or idle this is always false — a no-op guard.
     */
    fun isCapturingOwnVoice(): Boolean {
        val now = SystemClock.elapsedRealtime()
        // Bound `speaking` by wall-clock: if the engine ever drops the
        // terminal onDone/onError (documented to happen on some engines /
        // Samsung), an unbounded `speaking` would keep the mic muted for the
        // whole session. The cap lets it self-heal.
        val speakingNow = speaking && (now - speakingSinceMs) < MAX_UTTERANCE_MS
        return speakingNow || (now - lastSpokeEndMs) < TAIL_MARGIN_MS
    }

    private fun utteranceId(msg: String): String = "awareness-${msg.hashCode()}"

    private const val TAG = "Tts"
    // Covers the recognizer's ~2s complete-silence finalisation plus
    // speaker→mic acoustic latency, so the utterance that captured our own
    // alert still lands inside the muted window when it finalises.
    private const val TAIL_MARGIN_MS = 3000L
    // Upper bound on how long `speaking` is trusted without a terminal
    // callback — self-heals a dropped onDone/onError so a stuck flag can't
    // mute the mic for the whole session. Generous for a ≤240-char utterance.
    private const val MAX_UTTERANCE_MS = 30_000L
    // Matches the budget-conscious truncation on the desktop (tts.rs
    // shortens alerts to ~240 chars so they stay under a sentence).
    private const val MAX_CHARS = 240
}
