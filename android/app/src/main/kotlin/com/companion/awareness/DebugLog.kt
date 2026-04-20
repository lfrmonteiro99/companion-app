package com.companion.awareness

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import org.json.JSONObject
import java.io.File
import java.io.PrintWriter
import java.io.StringWriter
import java.time.Instant

/**
 * In-app diagnostic log buffer. Mirrors what ends up in logcat so the
 * user can diagnose crashes without adb. Also installs an uncaught
 * exception handler — when the process is about to die, we flush the
 * stack trace to disk so it shows up the next time the user opens the
 * Logs screen. The buffer is shared across the app via [entries].
 */
object DebugLog {
    private const val FILE_NAME = "debug.jsonl"
    private const val MAX_IN_MEMORY = 500
    private const val MAX_FILE_BYTES = 256 * 1024L

    enum class Level { D, I, W, E }

    data class Entry(
        val timestamp: String,
        val level: Level,
        val tag: String,
        val message: String,
        val stack: String? = null,
    )

    private val _entries = MutableStateFlow<List<Entry>>(emptyList())
    val entries: StateFlow<List<Entry>> get() = _entries.asStateFlow()

    @Volatile private var appContext: Context? = null

    fun init(ctx: Context) {
        if (appContext != null) return
        appContext = ctx.applicationContext
        _entries.value = readFromDisk(ctx.applicationContext)
    }

    @Volatile private var crashHandlerInstalled = false

    fun installCrashHandler(ctx: Context) {
        if (crashHandlerInstalled) return
        crashHandlerInstalled = true
        val app = ctx.applicationContext
        val previous = Thread.getDefaultUncaughtExceptionHandler()
        Thread.setDefaultUncaughtExceptionHandler { thread, throwable ->
            runCatching {
                writeEntry(
                    app,
                    Entry(
                        timestamp = Instant.now().toString(),
                        level = Level.E,
                        tag = "UncaughtException",
                        message = "Thread ${thread.name}: ${throwable.javaClass.simpleName}: ${throwable.message}",
                        stack = stackToString(throwable),
                    ),
                    toLogcat = false,
                )
            }
            previous?.uncaughtException(thread, throwable)
        }
    }

    fun d(tag: String, msg: String) = log(Level.D, tag, msg, null)
    fun i(tag: String, msg: String) = log(Level.I, tag, msg, null)
    fun w(tag: String, msg: String, t: Throwable? = null) = log(Level.W, tag, msg, t)
    fun e(tag: String, msg: String, t: Throwable? = null) = log(Level.E, tag, msg, t)

    fun clear(ctx: Context) {
        val app = ctx.applicationContext
        runCatching { File(app.filesDir, FILE_NAME).delete() }
        _entries.value = emptyList()
    }

    private fun log(level: Level, tag: String, msg: String, t: Throwable?) {
        val entry = Entry(
            timestamp = Instant.now().toString(),
            level = level,
            tag = tag,
            message = msg,
            stack = t?.let { stackToString(it) },
        )
        when (level) {
            Level.D -> android.util.Log.d(tag, msg, t)
            Level.I -> android.util.Log.i(tag, msg, t)
            Level.W -> android.util.Log.w(tag, msg, t)
            Level.E -> android.util.Log.e(tag, msg, t)
        }
        appContext?.let { writeEntry(it, entry, toLogcat = false) }
    }

    @Synchronized
    private fun writeEntry(ctx: Context, entry: Entry, toLogcat: Boolean) {
        val next = (_entries.value + entry).takeLast(MAX_IN_MEMORY)
        _entries.value = next

        runCatching {
            val file = File(ctx.filesDir, FILE_NAME)
            if (file.exists() && file.length() > MAX_FILE_BYTES) {
                // Rotate: keep only the most recent half.
                val kept = next.takeLast(MAX_IN_MEMORY / 2)
                file.writeText(kept.joinToString("\n", postfix = "\n") { toJson(it) })
            } else {
                file.appendText(toJson(entry) + "\n")
            }
        }
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
                            level = runCatching { Level.valueOf(o.optString("level", "I")) }
                                .getOrDefault(Level.I),
                            tag = o.optString("tag"),
                            message = o.optString("message"),
                            stack = if (o.isNull("stack")) null else o.optString("stack"),
                        )
                    )
                }
            }
        }
        return out.takeLast(MAX_IN_MEMORY)
    }

    private fun toJson(entry: Entry): String = JSONObject().apply {
        put("timestamp", entry.timestamp)
        put("level", entry.level.name)
        put("tag", entry.tag)
        put("message", entry.message)
        put("stack", entry.stack ?: JSONObject.NULL)
    }.toString()

    private fun stackToString(t: Throwable): String {
        val sw = StringWriter()
        t.printStackTrace(PrintWriter(sw))
        return sw.toString()
    }
}
