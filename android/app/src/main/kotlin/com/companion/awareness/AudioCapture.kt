package com.companion.awareness

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import androidx.core.content.ContextCompat
import java.util.concurrent.atomic.AtomicReference

/**
 * Mic capture stub. Produces raw PCM buffers; a future iteration should
 * pass them through VAD and a speech-to-text engine (e.g. on-device
 * Whisper via whisper.cpp, or the system SpeechRecognizer) and expose
 * the resulting text via [drainTranscript].
 */
class AudioCapture(private val ctx: Context) {
    private var record: AudioRecord? = null
    private var thread: Thread? = null
    private val pending = AtomicReference<String?>(null)

    @Volatile private var running = false

    fun start() {
        val granted = ContextCompat.checkSelfPermission(
            ctx, Manifest.permission.RECORD_AUDIO,
        ) == PackageManager.PERMISSION_GRANTED
        if (!granted) return

        val sampleRate = 16_000
        val minBuf = AudioRecord.getMinBufferSize(
            sampleRate,
            AudioFormat.CHANNEL_IN_MONO,
            AudioFormat.ENCODING_PCM_16BIT,
        )
        @Suppress("MissingPermission")
        record = AudioRecord(
            MediaRecorder.AudioSource.MIC,
            sampleRate,
            AudioFormat.CHANNEL_IN_MONO,
            AudioFormat.ENCODING_PCM_16BIT,
            minBuf * 2,
        )

        running = true
        record?.startRecording()
        thread = Thread {
            val buf = ShortArray(minBuf)
            while (running) {
                val n = record?.read(buf, 0, buf.size) ?: -1
                if (n <= 0) continue
                // TODO: run VAD + STT. For now we just count frames so the
                // pipeline end-to-end works; a real implementation would
                // emit recognised phrases here.
            }
        }.also { it.start() }
    }

    fun drainTranscript(): String? = pending.getAndSet(null)

    fun stop() {
        running = false
        thread?.join(500)
        record?.stop()
        record?.release()
        record = null
    }
}
