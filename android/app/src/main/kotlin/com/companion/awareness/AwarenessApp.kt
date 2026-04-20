package com.companion.awareness

import android.app.Application

/**
 * Hooks AppLog up before any other component runs. MainActivity.onCreate
 * is too late for crashes in Application-level initialisers or for
 * exceptions that fire before the first activity.
 */
class AwarenessApp : Application() {
    override fun onCreate() {
        super.onCreate()
        AppLog.attach(this)
        TraceLog.attach(this)
        AppLog.installCrashHandler()
        AppLog.i("AwarenessApp", "process start")
    }
}
