package com.companion.awareness

import android.content.Context
import org.json.JSONObject
import java.io.File
import java.time.Instant

/**
 * Append-only JSONL log of alerts we've shown to the user. Kept inside
 * `filesDir/alerts.jsonl`, so it survives service restarts. Mirrors the
 * desktop's `jsonl::JsonlWriter` + `memory::MemoryEntry` combo at the
 * byte level — same field names — so a future consolidation tool could
 * ingest both desktop and mobile logs.
 */
object AlertLog {
    private const val FILE_NAME = "alerts.jsonl"
    private const val MAX_ENTRIES_RETURNED = 200

    data class Entry(
        val timestamp: String,
        val app: String?,
        val alertType: String,
        val urgency: String,
        val quickMessage: String,
    )

    fun append(ctx: Context, entry: Entry) {
        val line = JSONObject().apply {
            put("timestamp", entry.timestamp)
            put("app", entry.app ?: JSONObject.NULL)
            put("alert_type", entry.alertType)
            put("urgency", entry.urgency)
            put("quick_message", entry.quickMessage)
        }.toString()
        runCatching {
            File(ctx.filesDir, FILE_NAME).appendText(line + "\n")
        }.onFailure { t ->
            android.util.Log.e("AlertLog", "append failed", t)
        }
    }

    /** Most recent entries first, capped at [MAX_ENTRIES_RETURNED]. */
    fun recent(ctx: Context): List<Entry> {
        val file = File(ctx.filesDir, FILE_NAME)
        if (!file.exists()) return emptyList()
        val out = ArrayList<Entry>()
        file.useLines { lines ->
            lines.forEach { line ->
                if (line.isBlank()) return@forEach
                runCatching {
                    val o = JSONObject(line)
                    out.add(
                        Entry(
                            timestamp = o.optString("timestamp", Instant.EPOCH.toString()),
                            app = if (o.isNull("app")) null else o.optString("app", null),
                            alertType = o.optString("alert_type", ""),
                            urgency = o.optString("urgency", "low"),
                            quickMessage = o.optString("quick_message", ""),
                        )
                    )
                }
            }
        }
        return out.asReversed().take(MAX_ENTRIES_RETURNED)
    }
}
