package com.companion.awareness

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import androidx.core.app.NotificationCompat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/**
 * Foreground service that drives screen + mic capture, runs OCR / VAD,
 * and hands context events to the Rust core for gating + API calls.
 *
 * This is a scaffold: ScreenCapture + AudioCapture are stubs that need
 * filling in with MediaProjection/VirtualDisplay + AudioRecord, plus
 * ML Kit text recognition on captured frames.
 */
class AwarenessService : Service() {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private var loopJob: Job? = null
    private var screen: ScreenCapture? = null
    private var audio: AudioCapture? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        startForegroundWithType()

        val resultCode = intent?.getIntExtra(EXTRA_RESULT_CODE, 0) ?: 0
        val data: Intent? = intent?.getParcelableExtra(EXTRA_DATA)
        if (resultCode != 0 && data != null) {
            screen = ScreenCapture(this, resultCode, data).also { it.start() }
        }
        audio = AudioCapture(this).also { it.start() }

        loopJob?.cancel()
        loopJob = scope.launch { runTickLoop() }
        return START_STICKY
    }

    private suspend fun runTickLoop() {
        // Matches the desktop cadence loosely; tune to battery on device.
        while (true) {
            val screenText = screen?.latestText().orEmpty()
            val micText = audio?.drainTranscript()
            val payload = """
                {"app":null,"window_title":null,
                 "screen_text":${jsonStr(screenText)},
                 "mic_text":${micText?.let { jsonStr(it) } ?: "null"},
                 "duration_on_app_seconds":0}
            """.trimIndent()
            val response = CoreBridge.submitContext(payload)
            android.util.Log.i("AwarenessService", "core -> $response")
            delay(30_000)
        }
    }

    private fun jsonStr(s: String): String =
        "\"" + s.replace("\\", "\\\\").replace("\"", "\\\"").replace("\n", "\\n") + "\""

    override fun onDestroy() {
        loopJob?.cancel()
        scope.cancel()
        screen?.stop()
        audio?.stop()
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun startForegroundWithType() {
        val nm = getSystemService(NOTIFICATION_SERVICE) as NotificationManager
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            nm.createNotificationChannel(
                NotificationChannel(
                    CHANNEL_ID,
                    getString(R.string.channel_name),
                    NotificationManager.IMPORTANCE_LOW,
                )
            )
        }
        val notif: Notification = NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle(getString(R.string.notification_title))
            .setContentText(getString(R.string.notification_text))
            .setSmallIcon(android.R.drawable.ic_menu_view)
            .setOngoing(true)
            .build()

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(
                NOTIF_ID,
                notif,
                ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION or
                    ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE,
            )
        } else {
            startForeground(NOTIF_ID, notif)
        }
    }

    companion object {
        private const val CHANNEL_ID = "awareness_capture"
        private const val NOTIF_ID = 1
        private const val EXTRA_RESULT_CODE = "result_code"
        private const val EXTRA_DATA = "data"

        fun start(ctx: Context, resultCode: Int, data: Intent) {
            val i = Intent(ctx, AwarenessService::class.java)
                .putExtra(EXTRA_RESULT_CODE, resultCode)
                .putExtra(EXTRA_DATA, data)
            ctx.startForegroundService(i)
        }

        fun stop(ctx: Context) {
            ctx.stopService(Intent(ctx, AwarenessService::class.java))
        }
    }
}
