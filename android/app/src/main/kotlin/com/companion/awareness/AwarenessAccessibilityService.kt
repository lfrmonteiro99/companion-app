package com.companion.awareness

import android.accessibilityservice.AccessibilityService
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityNodeInfo
import java.util.concurrent.atomic.AtomicReference

/**
 * Optional accessibility service — when the user enables it in system
 * Settings > Accessibility > Awareness, we get the visible text of the
 * focused window pushed to us on every state or content change. This
 * gives us a cleaner and faster signal than OCR on a screen capture,
 * and also surfaces the window title (analogous to `a11y.rs` on Linux).
 *
 * When the service is NOT enabled, [latestText] returns null and the
 * tick loop falls back to ML Kit OCR on a MediaProjection frame.
 *
 * Privacy note: Android warns the user clearly what accessibility
 * services can observe. This service does NOT persist the tree — we
 * only hold the most recent `full_text` in memory, overwritten each
 * event.
 */
class AwarenessAccessibilityService : AccessibilityService() {

    override fun onAccessibilityEvent(event: AccessibilityEvent?) {
        if (event == null) return
        when (event.eventType) {
            AccessibilityEvent.TYPE_WINDOW_STATE_CHANGED,
            AccessibilityEvent.TYPE_WINDOW_CONTENT_CHANGED,
            AccessibilityEvent.TYPE_VIEW_TEXT_CHANGED,
            AccessibilityEvent.TYPE_VIEW_FOCUSED,
            -> {
                val root = rootInActiveWindow ?: return
                val sb = StringBuilder()
                walk(root, sb, depth = 0)
                latest.set(Snapshot(
                    packageName = event.packageName?.toString(),
                    windowTitle = event.className?.toString(),
                    text = sb.toString().trim(),
                ))
            }
            else -> { /* ignore */ }
        }
    }

    private fun walk(node: AccessibilityNodeInfo?, out: StringBuilder, depth: Int) {
        if (node == null || depth > MAX_DEPTH) return
        val txt = node.text?.toString()?.takeIf { it.isNotBlank() }
        val desc = node.contentDescription?.toString()?.takeIf { it.isNotBlank() }
        if (txt != null) out.append(txt).append('\n')
        else if (desc != null) out.append(desc).append('\n')
        for (i in 0 until node.childCount) {
            walk(node.getChild(i), out, depth + 1)
        }
    }

    override fun onInterrupt() {
        // No long-running work to interrupt.
    }

    override fun onServiceConnected() {
        super.onServiceConnected()
        connected = true
    }

    override fun onDestroy() {
        connected = false
        latest.set(null)
        super.onDestroy()
    }

    data class Snapshot(
        val packageName: String?,
        val windowTitle: String?,
        val text: String,
    )

    companion object {
        private const val MAX_DEPTH = 40
        @Volatile private var connected = false
        private val latest = AtomicReference<Snapshot?>(null)

        fun isConnected(): Boolean = connected

        /** Returns the most recent snapshot, or null if the service is off. */
        fun latest(): Snapshot? = latest.get()
    }
}
