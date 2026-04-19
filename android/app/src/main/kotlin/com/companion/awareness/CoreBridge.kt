package com.companion.awareness

/**
 * Kotlin side of the JNI bridge to the shared Rust core.
 *
 * Native library `libawareness_core.so` is produced by
 * `android/core-rs/build.sh` (which runs cargo-ndk) and lands in
 * `app/src/main/jniLibs/<abi>/`.
 */
object CoreBridge {
    init {
        System.loadLibrary("awareness_core")
    }

    /** One-time logging setup inside the core. Safe to call twice. */
    external fun init()

    /**
     * Store the OpenAI API key + budget for this process. Must be called
     * before [analyze]. If the service is killed, call again on restart —
     * the key itself should live in `EncryptedSharedPreferences`.
     */
    external fun configure(apiKey: String, budgetUsdDaily: Double)

    /**
     * Submit a [com.companion.awareness.types.ContextEvent]-shaped JSON
     * and receive a [com.companion.awareness.types.FilterResponse]-shaped
     * JSON back. Runs the OpenAI filter call inside the core's tokio
     * runtime; blocks the caller thread, so invoke from a background
     * coroutine.
     */
    external fun analyze(eventJson: String): String
}
