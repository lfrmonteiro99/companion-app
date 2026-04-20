package com.companion.awareness

import android.app.NotificationManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/**
 * Receives notification action taps ("Mais disto" / "Não interessa")
 * and feeds them to the Rust user-profile learner. Also dismisses the
 * notification so the user gets immediate feedback that the action
 * registered.
 */
class RatingReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val topic = intent.getStringExtra(EXTRA_TOPIC)?.takeIf { it.isNotBlank() } ?: return
        val positive = intent.getBooleanExtra(EXTRA_POSITIVE, true)
        val notifId = intent.getIntExtra(EXTRA_NOTIF_ID, -1)

        runCatching {
            CoreBridge.learnInterest(topic, positive)
            AppLog.i(
                "RatingReceiver",
                "learnt ${if (positive) "+" else "-"}: ${topic.take(80)}",
            )
        }.onFailure { t -> AppLog.w("RatingReceiver", "learnInterest failed", t) }

        if (notifId > 0) {
            val nm = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
            nm.cancel(notifId)
        }
    }

    companion object {
        const val ACTION_RATE = "com.companion.awareness.ACTION_RATE"
        const val EXTRA_TOPIC = "topic"
        const val EXTRA_POSITIVE = "positive"
        const val EXTRA_NOTIF_ID = "notif_id"
    }
}
