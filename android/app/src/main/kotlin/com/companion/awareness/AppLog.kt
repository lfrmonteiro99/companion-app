package com.companion.awareness

import android.content.Context
import org.json.JSONObject
import java.io.File
import java.io.PrintWriter
import java.io.StringWriter
import java.time.Instant
import java.util.concurrent.atomic.AtomicReference

/**
 * Persistent in-app log so users can read crashes and warnings without
 * plugging the phone into adb. Mirrors AlertLog's JSONL pattern — one
 * entry per line in `filesDir/app_log.jsonl`. Both the explicit `w`/`e`
 * helpers AND the uncaught-exception handler installed by
 * [installCrashHandler] write here.
 *
 * Call [attach] once at process start (from Application.onCreate) so the
 * log file path is available before anything else runs.
 */
object AppLog {
    private const val FILE_NAME = "app_log.jsonl"
    private const val MAX_ENTRIES_RETURNED = 500
    private const val MAX_FILE_BYTES = 1_000_000L // ~1 MB → truncate from the head

    private val appContext = AtomicReference<Context?>(null)

    enum class Level { INFO, WARN, ERROR, CRASH }

    data class Entry(
        val timestamp: String,
        val level: Level,
        val tag: String,
        val message: String,
        val stackTrace: String?,
    )

    fun attach(ctx: Context) {
        appContext.compareAndSet(null, ctx.applicationContext)
    }

    fun i(tag: String, message: String) = append(Level.INFO, tag, message, null)
    fun w(tag: String, message: String, t: Throwable? = null) =
        append(Level.WARN, tag, message, t)
    fun e(tag: String, message: String, t: Throwable? = null) =
        append(Level.ERROR, tag, message, t)

    /** Invoked from the uncaught exception handler before the process dies. */
    internal fun crash(tag: String, t: Throwable) = append(Level.CRASH, tag, t.message ?: "uncaught", t)

    private fun append(level: Level, tag: String, message: String, t: Throwable?) {
        // Always mirror to logcat so adb logcat still works for developers.
        when (level) {
            Level.INFO -> android.util.Log.i(tag, message, t)
            Level.WARN -> android.util.Log.w(tag, message, t)
            Level.ERROR -> android.util.Log.e(tag, message, t)
            Level.CRASH -> android.util.Log.e(tag, "CRASH: $message", t)
        }

        val ctx = appContext.get() ?: return
        val stack = t?.let {
            val sw = StringWriter()
            it.printStackTrace(PrintWriter(sw))
            sw.toString()
        }
        val line = JSONObject().apply {
            put("timestamp", Instant.now().toString())
            put("level", level.name)
            put("tag", tag)
            put("message", message)
            if (stack != null) put("stack", stack)
        }.toString()

        runCatching {
            val file = File(ctx.filesDir, FILE_NAME)
            // Cheap cap: if we've grown past MAX_FILE_BYTES, drop the oldest
            // half. Avoids ballooning storage during a crash-loop.
            if (file.exists() && file.length() > MAX_FILE_BYTES) {
                val all = file.readLines()
                file.writeText(all.takeLast(all.size / 2).joinToString("\n", postfix = "\n"))
            }
            file.appendText(line + "\n")
        }.onFailure { err ->
            android.util.Log.e("AppLog", "append failed", err)
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
                            level = runCatching { Level.valueOf(o.optString("level", "INFO")) }
                                .getOrDefault(Level.INFO),
                            tag = o.optString("tag", "?"),
                            message = o.optString("message", ""),
                            stackTrace = if (o.has("stack")) o.optString("stack") else null,
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

    /**
     * Install a JVM-wide uncaught exception handler that persists the crash
     * to the log file before delegating to the previous handler (which
     * normally kills the process and shows the "Awareness keeps stopping"
     * dialog). Call once, as early as possible.
     */
    fun installCrashHandler() {
        val previous = Thread.getDefaultUncaughtExceptionHandler()
        Thread.setDefaultUncaughtExceptionHandler { thread, throwable ->
            runCatching { crash(thread.name.ifEmpty { "uncaught" }, throwable) }
            previous?.uncaughtException(thread, throwable)
        }
    }
}
