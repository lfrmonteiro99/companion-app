package com.companion.awareness

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
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
import org.json.JSONObject
import java.time.Instant
import java.util.concurrent.atomic.AtomicInteger

/**
 * Foreground service that drives screen + mic capture, calls into the
 * shared Rust core for analysis, and posts notifications when the model
 * decides the user should be alerted.
 */
class AwarenessService : Service() {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private var loopJob: Job? = null
    private var screen: ScreenCapture? = null
    private var audio: AudioCapture? = null
    private val alertCounter = AtomicInteger(ALERT_ID_BASE)

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        startForegroundWithType()
        ensureAlertChannel()

        val resultCode = intent?.getIntExtra(EXTRA_RESULT_CODE, 0) ?: 0
        val data: Intent? = intent?.getParcelableExtra(EXTRA_DATA)
        if (resultCode != 0 && data != null) {
            screen = ScreenCapture(this, resultCode, data).also { it.start() }
        }
        audio = AudioCapture(this).also { it.start() }

        configureCoreFromStoredKey()

        loopJob?.cancel()
        loopJob = scope.launch { runTickLoop() }
        return START_STICKY
    }

    private fun configureCoreFromStoredKey() {
        val key = Settings.openAiKey(this)
        if (key.isBlank()) {
            android.util.Log.w(TAG, "no OpenAI key stored; analyze calls will fail")
            return
        }
        CoreBridge.configure(key, Settings.budgetUsdDaily(this), filesDir.absolutePath)
    }

    private suspend fun runTickLoop() {
        var appStartedAt = System.currentTimeMillis()
        var lastApp: String? = null
        while (true) {
            val currentApp = FocusedApp.currentPackage(this)
            if (currentApp != lastApp) {
                appStartedAt = System.currentTimeMillis()
                lastApp = currentApp
            }
            val durationSec = (System.currentTimeMillis() - appStartedAt) / 1000

            val screenText = screen?.latestText().orEmpty()
            val micText = audio?.drainTranscript()

            val eventJson = JSONObject().apply {
                put("timestamp", Instant.now().toString())
                put("app", currentApp ?: JSONObject.NULL)
                put("window_title", JSONObject.NULL)
                put("screen_text_excerpt", screenText.take(8000))
                put("mic_text_recent", micText ?: JSONObject.NULL)
                put("duration_on_app_seconds", durationSec)
                put("history_apps_30min", org.json.JSONArray())
                put("mic_text_new", micText != null)
            }.toString()

            try {
                val responseJson = CoreBridge.analyze(eventJson)
                handleResponse(responseJson)
            } catch (t: Throwable) {
                android.util.Log.e(TAG, "analyze failed", t)
            }

            delay(TICK_MS)
        }
    }

    private fun handleResponse(responseJson: String) {
        val obj = runCatching { JSONObject(responseJson) }.getOrNull() ?: return
        val shouldAlert = obj.optBoolean("should_alert", false)
        if (!shouldAlert) return

        val title = obj.optString("alert_type", "alert").replaceFirstChar { it.uppercase() }
        val body = obj.optString("quick_message", "")
        val urgency = obj.optString("urgency", "low")
        postAlert(title, body, urgency)
    }

    private fun postAlert(title: String, body: String, urgency: String) {
        val priority = when (urgency) {
            "high" -> NotificationCompat.PRIORITY_HIGH
            "medium" -> NotificationCompat.PRIORITY_DEFAULT
            else -> NotificationCompat.PRIORITY_LOW
        }
        val tap = PendingIntent.getActivity(
            this, 0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val notif = NotificationCompat.Builder(this, ALERT_CHANNEL_ID)
            .setContentTitle(title)
            .setContentText(body)
            .setStyle(NotificationCompat.BigTextStyle().bigText(body))
            .setSmallIcon(android.R.drawable.ic_dialog_info)
            .setPriority(priority)
            .setAutoCancel(true)
            .setContentIntent(tap)
            .build()
        val nm = getSystemService(NOTIFICATION_SERVICE) as NotificationManager
        nm.notify(alertCounter.incrementAndGet(), notif)
    }

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

    private fun ensureAlertChannel() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val nm = getSystemService(NOTIFICATION_SERVICE) as NotificationManager
        nm.createNotificationChannel(
            NotificationChannel(
                ALERT_CHANNEL_ID,
                "Awareness alerts",
                NotificationManager.IMPORTANCE_DEFAULT,
            )
        )
    }

    companion object {
        private const val TAG = "AwarenessService"
        private const val CHANNEL_ID = "awareness_capture"
        private const val ALERT_CHANNEL_ID = "awareness_alerts"
        private const val NOTIF_ID = 1
        private const val ALERT_ID_BASE = 100
        private const val TICK_MS = 30_000L
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
