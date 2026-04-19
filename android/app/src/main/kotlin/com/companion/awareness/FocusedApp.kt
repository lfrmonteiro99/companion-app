package com.companion.awareness

import android.app.AppOpsManager
import android.app.usage.UsageStatsManager
import android.content.Context
import android.os.Build
import android.os.Process

/**
 * Helper that returns the package name of the app the user is currently
 * focused on, using [UsageStatsManager]. Requires the user to grant
 * "Usage access" in system Settings — not a runtime permission dialog,
 * we have to open the dedicated settings page (see [isGranted] and the
 * intent in `MainActivity`).
 *
 * Returns null when:
 *   - permission not granted yet,
 *   - no foreground event in the last minute (device idle, screen off),
 *   - older than API 22 (we ship min SDK 29, so this branch is dead).
 *
 * The desktop version derives `event.app` from a11y / window title. On
 * Android this is the cleanest equivalent without requiring a full
 * AccessibilityService, which demands a more intrusive user opt-in.
 */
object FocusedApp {

    fun isGranted(ctx: Context): Boolean {
        val appOps = ctx.getSystemService(Context.APP_OPS_SERVICE) as AppOpsManager
        val mode = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            appOps.unsafeCheckOpNoThrow(
                AppOpsManager.OPSTR_GET_USAGE_STATS,
                Process.myUid(),
                ctx.packageName,
            )
        } else {
            @Suppress("DEPRECATION")
            appOps.checkOpNoThrow(
                AppOpsManager.OPSTR_GET_USAGE_STATS,
                Process.myUid(),
                ctx.packageName,
            )
        }
        return mode == AppOpsManager.MODE_ALLOWED
    }

    /** Returns the foreground package name, or null if unavailable. */
    fun currentPackage(ctx: Context): String? {
        if (!isGranted(ctx)) return null
        val usm = ctx.getSystemService(Context.USAGE_STATS_SERVICE) as? UsageStatsManager
            ?: return null

        val now = System.currentTimeMillis()
        val events = usm.queryEvents(now - 60_000L, now)
        val ev = android.app.usage.UsageEvents.Event()
        var lastForeground: String? = null
        while (events.hasNextEvent()) {
            events.getNextEvent(ev)
            if (ev.eventType == android.app.usage.UsageEvents.Event.ACTIVITY_RESUMED ||
                ev.eventType == android.app.usage.UsageEvents.Event.MOVE_TO_FOREGROUND
            ) {
                lastForeground = ev.packageName
            }
        }
        // Filter our own package — we don't want alerts about the
        // awareness app itself while the user is configuring it.
        return lastForeground?.takeIf { it != ctx.packageName }
    }
}
