package com.companion.awareness

import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import androidx.core.app.NotificationCompat
import androidx.work.CoroutineWorker
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import java.util.concurrent.TimeUnit

/**
 * Dead-man's-switch for the capture pipeline.
 *
 * Samsung One UI (and other aggressive OEM power managers) silently kill
 * our foreground service + process even when the battery whitelist is on
 * and a partial wake lock is held. When that happens, `onDestroy` is NOT
 * called, `MediaProjection.Callback.onStop` is NOT called, the user just
 * sees the persistent notification vanish and has no idea capture stopped.
 *
 * This watchdog rides on WorkManager, which the OS explicitly re-animates
 * after killing our process. Flow:
 *
 *  1. AwarenessService.onStartCommand calls `markCaptureExpected(true)`
 *     and enqueues a periodic worker (every ~15 min, the platform
 *     minimum).
 *  2. Every tick of runTickLoop calls `recordHeartbeat()`.
 *  3. AwarenessService.onDestroy (voluntary stop) calls
 *     `markCaptureExpected(false)` and cancels the worker.
 *  4. `LivenessWorker` runs in its own process, compares the stored
 *     heartbeat timestamp against now. If capture is expected but the
 *     heartbeat is older than STALE_THRESHOLD_MS, it posts a
 *     "Capture died silently — tap to resume" notification and flips
 *     `captureExpected` off so we don't spam.
 */
object LivenessWatchdog {
    private const val PREFS = "awareness_watchdog"
    private const val KEY_LAST_HEARTBEAT = "last_heartbeat_ms"
    private const val KEY_CAPTURE_EXPECTED = "capture_expected"
    private const val WORK_NAME = "awareness-liveness"
    private const val ALERT_CHANNEL_ID = "awareness_alerts"
    /** A heartbeat older than this means the service died silently. Tick
     *  interval is 10 s, so 90 s = 9 missed ticks is unambiguous. */
    private const val STALE_THRESHOLD_MS = 90_000L

    fun markCaptureExpected(ctx: Context, expected: Boolean) {
        ctx.prefs().edit()
            .putBoolean(KEY_CAPTURE_EXPECTED, expected)
            .putLong(KEY_LAST_HEARTBEAT, if (expected) System.currentTimeMillis() else 0L)
            .apply()
        if (expected) enqueuePeriodic(ctx) else cancel(ctx)
    }

    fun recordHeartbeat(ctx: Context) {
        ctx.prefs().edit().putLong(KEY_LAST_HEARTBEAT, System.currentTimeMillis()).apply()
    }

    private fun enqueuePeriodic(ctx: Context) {
        // 15 min is the platform-enforced minimum interval for periodic
        // work. It's coarser than we'd like (a Samsung kill can go
        // unnoticed for up to 15 min) but that's the trade-off for a
        // scheduler that actually survives process death. Good enough as
        // a "your app is dead" breadcrumb.
        val work = PeriodicWorkRequestBuilder<LivenessWorker>(15, TimeUnit.MINUTES)
            .build()
        WorkManager.getInstance(ctx).enqueueUniquePeriodicWork(
            WORK_NAME,
            ExistingPeriodicWorkPolicy.UPDATE,
            work,
        )
    }

    private fun cancel(ctx: Context) {
        WorkManager.getInstance(ctx).cancelUniqueWork(WORK_NAME)
    }

    private fun Context.prefs() =
        getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    class LivenessWorker(ctx: Context, params: WorkerParameters) :
        CoroutineWorker(ctx, params) {
        override suspend fun doWork(): Result {
            val prefs = applicationContext.prefs()
            val expected = prefs.getBoolean(KEY_CAPTURE_EXPECTED, false)
            if (!expected) return Result.success()

            val last = prefs.getLong(KEY_LAST_HEARTBEAT, 0L)
            val age = System.currentTimeMillis() - last
            if (age < STALE_THRESHOLD_MS) return Result.success()

            AppLog.w(
                "LivenessWatchdog",
                "capture died silently (last heartbeat ${age / 1000}s ago) — notifying user",
            )
            postRestartNotification()
            // Flip the flag so we don't keep re-posting every 15 min until
            // the user taps. The next Start press sets it true again.
            prefs.edit().putBoolean(KEY_CAPTURE_EXPECTED, false).apply()
            return Result.success()
        }

        private fun postRestartNotification() {
            val ctx = applicationContext
            val intent = Intent(ctx, MainActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP)
                .putExtra(AwarenessService.EXTRA_AUTO_START, true)
            val tap = PendingIntent.getActivity(
                ctx, 2, intent,
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            )
            val notif = NotificationCompat.Builder(ctx, ALERT_CHANNEL_ID)
                .setContentTitle("Captura morreu em background")
                .setContentText("O sistema matou a app. Toca aqui para retomar a captura.")
                .setSmallIcon(android.R.drawable.ic_dialog_alert)
                .setPriority(NotificationCompat.PRIORITY_HIGH)
                .setAutoCancel(true)
                .setContentIntent(tap)
                .build()
            val nm = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
            nm.notify(42, notif)
        }
    }
}
