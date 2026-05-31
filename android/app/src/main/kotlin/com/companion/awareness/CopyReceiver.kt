package com.companion.awareness

import android.app.NotificationManager
import android.content.BroadcastReceiver
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.widget.Toast

/**
 * Receives the "Copiar resposta" notification action tap, puts the
 * suggested reply on the system clipboard, shows a short toast, and
 * cancels the notification — mirroring RatingReceiver's pattern exactly.
 */
class CopyReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val text = intent.getStringExtra(EXTRA_TEXT)?.takeIf { it.isNotBlank() } ?: return
        val notifId = intent.getIntExtra(EXTRA_NOTIF_ID, -1)

        runCatching {
            val clipboard =
                context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
            clipboard.setPrimaryClip(ClipData.newPlainText("suggested_reply", text))
            Toast.makeText(context, "Copiado", Toast.LENGTH_SHORT).show()
            AppLog.i("CopyReceiver", "copied reply (${text.length} chars)")
        }.onFailure { t -> AppLog.w("CopyReceiver", "copy failed", t) }

        if (notifId > 0) {
            val nm = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
            nm.cancel(notifId)
        }
    }

    companion object {
        const val ACTION_COPY = "com.companion.awareness.ACTION_COPY"
        const val EXTRA_TEXT = "text"
        const val EXTRA_NOTIF_ID = "notif_id"
    }
}
