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
        val resultCode = intent?.getIntExtra(EXTRA_RESULT_CODE, 0) ?: 0
        val data: Intent? = intent?.getParcelableExtra(EXTRA_DATA)

        // Android re-starts foreground services after the process dies.
        // That restart does NOT carry the MediaProjection token back — the
        // appop `android:project_media` is granted only while the user's
        // active consent is live, and expires when the process exits. If
        // we try to startForeground with FOREGROUND_SERVICE_TYPE_MEDIA_-
        // PROJECTION without that appop, Android 14 throws SecurityException
        // and the service crash-loops. Bail cleanly so the user has to
        // press Start again (we also return START_NOT_STICKY below so the
        // restart never happens in the first place).
        if (resultCode == 0 || data == null) {
            AppLog.w(TAG, "onStartCommand without MediaProjection token (likely a restart); stopping")
            stopSelf(startId)
            return START_NOT_STICKY
        }

        startForegroundWithType()
        ensureAlertChannel()

        screen = ScreenCapture(
            this,
            resultCode,
            data,
            onStopped = {
                // MainActivity picks up EXTRA_AUTO_START and replays the
                // permission + projection flow on launch, so a single tap
                // on this notification gets the user back to a running
                // service without extra clicks.
                val resumeIntent = Intent(this, MainActivity::class.java)
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP)
                    .putExtra(EXTRA_AUTO_START, true)
                val tap = PendingIntent.getActivity(
                    this, 1, resumeIntent,
                    PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
                )
                val notif = NotificationCompat.Builder(this, ALERT_CHANNEL_ID)
                    .setContentTitle("Captura parou")
                    .setContentText("Toca aqui para retomar a captura.")
                    .setSmallIcon(android.R.drawable.ic_dialog_alert)
                    .setPriority(NotificationCompat.PRIORITY_DEFAULT)
                    .setAutoCancel(true)
                    .setContentIntent(tap)
                    .build()
                val nm = getSystemService(NOTIFICATION_SERVICE) as NotificationManager
                nm.notify(alertCounter.incrementAndGet(), notif)
                stopSelf()
            },
        ).also { it.start() }
        // AudioRecord throws on construction without RECORD_AUDIO; the
        // service is allowed to start without mic, so skip audio wiring
        // when the user denied the permission.
        if (hasMicPermission()) {
            audio = AudioCapture(this).also { it.start() }
        } else {
            AppLog.i(TAG, "mic disabled — RECORD_AUDIO not granted")
        }
        if (Settings.ttsEnabled(this)) Tts.ensureStarted(this)

        configureCoreFromStoredKey()

        loopJob?.cancel()
        loopJob = scope.launch { runTickLoop() }
        // NOT_STICKY: don't let Android resurrect us without a fresh
        // MediaProjection grant. The user's Start button is the only
        // legitimate entry point.
        return START_NOT_STICKY
    }

    private fun configureCoreFromStoredKey() {
        val key = Settings.openAiKey(this)
        if (key.isBlank()) {
            AppLog.w(TAG, "no OpenAI key stored; analyze calls will fail")
            return
        }
        CoreBridge.configure(key, Settings.budgetUsdDaily(this), filesDir.absolutePath)
    }

    private suspend fun runTickLoop() {
        var appStartedAt = System.currentTimeMillis()
        var lastApp: String? = null
        while (true) {
            val a11y = AwarenessAccessibilityService.latest()
            val currentApp = a11y?.packageName
                ?.takeIf { it != packageName }
                ?: FocusedApp.currentPackage(this)
            if (currentApp != lastApp) {
                appStartedAt = System.currentTimeMillis()
                lastApp = currentApp
            }
            val durationSec = (System.currentTimeMillis() - appStartedAt) / 1000

            // Prefer accessibility text (cleaner + faster) when the
            // service is enabled; fall back to ML Kit OCR otherwise.
            val screenText = a11y?.text?.takeIf { it.isNotBlank() }
                ?: screen?.latestText().orEmpty()
            val windowTitle = a11y?.windowTitle
            val micText = audio?.drainTranscript()

            // Self-observation guard. The OCR pass sometimes captures
            // the notification shade or our own LogsActivity, so the text
            // contains our own package name + crash traces. Feeding that
            // to the model just produces alerts *about* our previous
            // alerts, in a tightening loop. Drop those ticks entirely.
            val looksSelfReferential = currentApp == packageName ||
                screenText.contains("com.companion.awareness") ||
                screenText.contains("AwarenessApp") ||
                screenText.contains("awareness-core")
            if (looksSelfReferential) {
                delay(TICK_MS)
                continue
            }

            val eventJson = JSONObject().apply {
                put("timestamp", Instant.now().toString())
                put("app", currentApp ?: JSONObject.NULL)
                put("window_title", windowTitle ?: JSONObject.NULL)
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
                AppLog.e(TAG, "analyze failed", t)
            }

            delay(TICK_MS)
        }
    }

    private fun handleResponse(responseJson: String) {
        val obj = runCatching { JSONObject(responseJson) }.getOrNull() ?: return
        val shouldAlert = obj.optBoolean("should_alert", false)
        if (!shouldAlert) return

        val alertType = obj.optString("alert_type", "alert")
        val title = alertType.replaceFirstChar { it.uppercase() }
        val body = obj.optString("quick_message", "")
        val urgency = obj.optString("urgency", "low")
        postAlert(title, body, urgency)

        AlertLog.append(
            this,
            AlertLog.Entry(
                timestamp = Instant.now().toString(),
                app = FocusedApp.currentPackage(this)
                    ?: AwarenessAccessibilityService.latest()?.packageName,
                alertType = alertType,
                urgency = urgency,
                quickMessage = body,
            ),
        )
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

        if (Settings.ttsEnabled(this)) Tts.speak(body)
    }

    override fun onDestroy() {
        loopJob?.cancel()
        scope.cancel()
        screen?.stop()
        audio?.stop()
        Tts.shutdown()
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun hasMicPermission(): Boolean =
        androidx.core.content.ContextCompat.checkSelfPermission(
            this,
            android.Manifest.permission.RECORD_AUDIO,
        ) == android.content.pm.PackageManager.PERMISSION_GRANTED

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
            // Android 14+ rejects fgServiceType=microphone if RECORD_AUDIO
            // isn't granted yet — SecurityException kills the process with
            // "Awareness keeps stopping". Compose the type mask dynamically
            // so the service starts in media-projection-only mode when the
            // user declines mic access; audio capture gracefully no-ops.
            var type = ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION
            if (hasMicPermission()) {
                type = type or ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
            } else {
                AppLog.w(TAG, "RECORD_AUDIO not granted — starting without mic fgServiceType")
            }
            startForeground(NOTIF_ID, notif, type)
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
        // 10s mirrors the Linux CLI default; the gate's server-side
        // dedup keeps API cost bounded. 30s made the app feel dead
        // because the first analysis was half a minute away.
        private const val TICK_MS = 10_000L
        private const val EXTRA_RESULT_CODE = "result_code"
        private const val EXTRA_DATA = "data"
        /** MainActivity reads this from its launching intent and replays
         *  the permission + MediaProjection flow automatically, so the
         *  "Captura parou" notification is one-tap resume. */
        const val EXTRA_AUTO_START = "auto_start"

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
