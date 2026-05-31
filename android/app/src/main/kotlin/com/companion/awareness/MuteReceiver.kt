package com.companion.awareness

import android.app.NotificationManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.widget.Toast
import java.util.concurrent.TimeUnit

/**
 * Receives the "Silenciar 1h" notification action tap. Persists a
 * per-app mute-until epoch-millis in SharedPreferences keyed by app
 * package name, cancels the notification, and shows a short toast —
 * mirroring RatingReceiver's pattern exactly.
 */
class MuteReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val appPkg = intent.getStringExtra(EXTRA_APP)?.takeIf { it.isNotBlank() } ?: return
        val notifId = intent.getIntExtra(EXTRA_NOTIF_ID, -1)

        runCatching {
            val muteUntil = System.currentTimeMillis() + TimeUnit.HOURS.toMillis(1)
            context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
                .edit()
                .putLong(muteKey(appPkg), muteUntil)
                .apply()
            Toast.makeText(context, "Silenciado 1h", Toast.LENGTH_SHORT).show()
            AppLog.i("MuteReceiver", "muted $appPkg until $muteUntil")
        }.onFailure { t -> AppLog.w("MuteReceiver", "mute failed", t) }

        if (notifId > 0) {
            val nm = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
            nm.cancel(notifId)
        }
    }

    companion object {
        const val ACTION_MUTE = "com.companion.awareness.ACTION_MUTE"
        const val EXTRA_APP = "app"
        const val EXTRA_NOTIF_ID = "notif_id"

        private const val PREFS_NAME = "awareness-mutes"

        fun muteKey(appPkg: String) = "mute_until:$appPkg"

        /** Returns true when [appPkg] is currently muted (now < mute-until). */
        fun isMuted(context: Context, appPkg: String): Boolean {
            if (appPkg.isBlank()) return false
            val muteUntil = context
                .getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
                .getLong(muteKey(appPkg), 0L)
            return System.currentTimeMillis() < muteUntil
        }
    }
}
