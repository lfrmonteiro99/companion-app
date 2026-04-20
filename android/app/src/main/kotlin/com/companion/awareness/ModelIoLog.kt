package com.companion.awareness

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import org.json.JSONObject
import java.io.File
import java.time.Instant

/**
 * Records each round-trip through [CoreBridge.analyze] so the user can
 * see, in-app, what context we're sending to the model and what the
 * model decided. Persisted as JSONL in `filesDir/model_io.jsonl` and
 * mirrored in a [StateFlow] that the UI observes. Capped so we don't
 * grow unbounded.
 */
object ModelIoLog {
    private const val FILE_NAME = "model_io.jsonl"
    private const val MAX_IN_MEMORY = 100
    private const val MAX_FILE_BYTES = 512 * 1024L

    data class Entry(
        val timestamp: String,
        val durationMs: Long,
        val requestJson: String,
        val responseJson: String?,
        val error: String?,
        // Surfaced for quick glance without parsing the full JSON.
        val app: String?,
        val windowTitle: String?,
        val shouldAlert: Boolean?,
        val alertType: String?,
        val urgency: String?,
        val quickMessage: String?,
    )

    private val _entries = MutableStateFlow<List<Entry>>(emptyList())
    val entries: StateFlow<List<Entry>> get() = _entries.asStateFlow()

    @Volatile private var appContext: Context? = null

    fun init(ctx: Context) {
        if (appContext != null) return
        appContext = ctx.applicationContext
        _entries.value = readFromDisk(ctx.applicationContext)
    }

    fun recordSuccess(ctx: Context, requestJson: String, responseJson: String, durationMs: Long) {
        val req = runCatching { JSONObject(requestJson) }.getOrNull()
        val res = runCatching { JSONObject(responseJson) }.getOrNull()
        append(
            ctx,
            Entry(
                timestamp = Instant.now().toString(),
                durationMs = durationMs,
                requestJson = requestJson,
                responseJson = responseJson,
                error = null,
                app = req?.optStringOrNull("app"),
                windowTitle = req?.optStringOrNull("window_title"),
                shouldAlert = res?.optBoolean("should_alert", false),
                alertType = res?.optStringOrNull("alert_type"),
                urgency = res?.optStringOrNull("urgency"),
                quickMessage = res?.optStringOrNull("quick_message"),
            ),
        )
    }

    fun recordError(ctx: Context, requestJson: String, error: String, durationMs: Long) {
        val req = runCatching { JSONObject(requestJson) }.getOrNull()
        append(
            ctx,
            Entry(
                timestamp = Instant.now().toString(),
                durationMs = durationMs,
                requestJson = requestJson,
                responseJson = null,
                error = error,
                app = req?.optStringOrNull("app"),
                windowTitle = req?.optStringOrNull("window_title"),
                shouldAlert = null,
                alertType = null,
                urgency = null,
                quickMessage = null,
            ),
        )
    }

    fun clear(ctx: Context) {
        val app = ctx.applicationContext
        runCatching { File(app.filesDir, FILE_NAME).delete() }
        _entries.value = emptyList()
    }

    @Synchronized
    private fun append(ctx: Context, entry: Entry) {
        val app = ctx.applicationContext
        val next = (_entries.value + entry).takeLast(MAX_IN_MEMORY)
        _entries.value = next
        runCatching {
            val file = File(app.filesDir, FILE_NAME)
            if (file.exists() && file.length() > MAX_FILE_BYTES) {
                val kept = next.takeLast(MAX_IN_MEMORY / 2)
                file.writeText(kept.joinToString("\n", postfix = "\n") { toJson(it) })
            } else {
                file.appendText(toJson(entry) + "\n")
            }
        }.onFailure { DebugLog.e("ModelIoLog", "persist failed", it) }
    }

    private fun readFromDisk(ctx: Context): List<Entry> {
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
                            timestamp = o.optString("timestamp"),
                            durationMs = o.optLong("duration_ms", 0),
                            requestJson = o.optString("request", ""),
                            responseJson = o.optStringOrNull("response"),
                            error = o.optStringOrNull("error"),
                            app = o.optStringOrNull("app"),
                            windowTitle = o.optStringOrNull("window_title"),
                            shouldAlert = if (o.isNull("should_alert")) null
                                else o.optBoolean("should_alert"),
                            alertType = o.optStringOrNull("alert_type"),
                            urgency = o.optStringOrNull("urgency"),
                            quickMessage = o.optStringOrNull("quick_message"),
                        )
                    )
                }
            }
        }
        return out.takeLast(MAX_IN_MEMORY)
    }

    private fun toJson(e: Entry): String = JSONObject().apply {
        put("timestamp", e.timestamp)
        put("duration_ms", e.durationMs)
        put("request", e.requestJson)
        put("response", e.responseJson ?: JSONObject.NULL)
        put("error", e.error ?: JSONObject.NULL)
        put("app", e.app ?: JSONObject.NULL)
        put("window_title", e.windowTitle ?: JSONObject.NULL)
        put("should_alert", e.shouldAlert ?: JSONObject.NULL)
        put("alert_type", e.alertType ?: JSONObject.NULL)
        put("urgency", e.urgency ?: JSONObject.NULL)
        put("quick_message", e.quickMessage ?: JSONObject.NULL)
    }.toString()

    private fun JSONObject.optStringOrNull(name: String): String? {
        if (isNull(name)) return null
        val s = optString(name, "")
        return s.ifBlank { null }
    }
}
