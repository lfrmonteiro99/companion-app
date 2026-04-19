package com.companion.awareness

/**
 * Kotlin side of the JNI bridge to the shared Rust core.
 *
 * The native library `libawareness_core.so` is produced by
 * `android/core-rs/build.sh` (which uses cargo-ndk) and dropped into
 * `app/src/main/jniLibs/<abi>/`.
 */
object CoreBridge {
    init {
        System.loadLibrary("awareness_core")
    }

    /** Initialises logging + the tokio runtime inside the core. */
    external fun init(): Long

    /**
     * Submits a context event (JSON) and receives a gating decision (JSON).
     * Schema matches [awareness_core::ContextInput] / [CoreResponse].
     */
    external fun submitContext(jsonInput: String): String
}
