package com.companion.awareness

import android.content.Context
import org.json.JSONObject
import java.io.File
import java.time.Instant
import java.util.concurrent.atomic.AtomicReference

/**
 * Per-tick pipeline trace. Answers "what did the service do this cycle?"
 * in plain language: captured X chars from app Y, gate said Send/Skip
 * because Z, OpenAI charged $N and returned alert=true/false, and the
 * notification was delivered or suppressed.
 *
 * Backed by JSONL in `filesDir/trace_log.jsonl`. Capped at ~400 KiB so it
 * never balloons during a busy session. Separate from AppLog, which is
 * for crashes/errors only.
 */
object TraceLog {
    private const val FILE_NAME = "trace_log.jsonl"
    private const val MAX_ENTRIES_RETURNED = 300
    private const val MAX_FILE_BYTES = 400_000L

    private val appContext = AtomicReference<Context?>(null)

    enum class Stage { CAPTURE, GATE_SKIP, GATE_SEND, API_RESP, BUDGET, NOTIFY_POST, NOTIFY_SUPPRESS, ANALYZE_FAIL }

    data class Entry(
        val timestamp: String,
        val tickId: Long,
        val stage: Stage,
        val text: String,
    )

    fun attach(ctx: Context) = appContext.compareAndSet(null, ctx.applicationContext).let { Unit }

    fun captured(tickId: Long, app: String?, chars: Int, hasMic: Boolean, preview: String) =
        write(tickId, Stage.CAPTURE, "app=${app ?: "?"} chars=$chars mic=$hasMic preview=${preview.take(60)}")

    fun gateSkip(tickId: Long, reason: String) =
        write(tickId, Stage.GATE_SKIP, "gate=Skip reason=$reason · NOT sending to OpenAI")

    fun gateSend(tickId: Long, reason: String) =
        write(tickId, Stage.GATE_SEND, "gate=Send reason=$reason · SENDING to OpenAI")

    fun apiResponse(tickId: Long, alertType: String, shouldAlert: Boolean, costUsd: Double, message: String) =
        write(
            tickId,
            Stage.API_RESP,
            "type=$alertType should_alert=$shouldAlert cost=${"$%.6f".format(costUsd)} msg=${message.take(140)}",
        )

    fun budgetExceeded(tickId: Long) =
        write(tickId, Stage.BUDGET, "daily budget exhausted · skipping API call")

    fun notificationPosted(tickId: Long, type: String, urgency: String) =
        write(tickId, Stage.NOTIFY_POST, "posted type=$type urgency=$urgency")

    fun notificationSuppressed(tickId: Long, reason: String) =
        write(tickId, Stage.NOTIFY_SUPPRESS, "suppressed · $reason")

    fun analyzeFail(tickId: Long, message: String) =
        write(tickId, Stage.ANALYZE_FAIL, message.take(240))

    private fun write(tickId: Long, stage: Stage, text: String) {
        val ctx = appContext.get() ?: return
        val line = JSONObject().apply {
            put("timestamp", Instant.now().toString())
            put("tick_id", tickId)
            put("stage", stage.name)
            put("text", text)
        }.toString()
        runCatching {
            val file = File(ctx.filesDir, FILE_NAME)
            if (file.exists() && file.length() > MAX_FILE_BYTES) {
                val all = file.readLines()
                file.writeText(all.takeLast(all.size / 2).joinToString("\n", postfix = "\n"))
            }
            file.appendText(line + "\n")
        }.onFailure { err ->
            android.util.Log.e("TraceLog", "append failed", err)
        }
    }

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
                            tickId = o.optLong("tick_id", 0),
                            stage = runCatching { Stage.valueOf(o.optString("stage", "CAPTURE")) }
                                .getOrDefault(Stage.CAPTURE),
                            text = o.optString("text", ""),
                        )
                    )
                }
            }
        }
        return out.asReversed().take(MAX_ENTRIES_RETURNED)
    }

    fun clear(ctx: Context) {
        runCatching { File(ctx.filesDir, FILE_NAME).delete() }
    }
}
