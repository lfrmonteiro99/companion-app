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
     * Store the OpenAI API key + daily USD budget + a writable directory
     * (the app's internal `filesDir`) where the core persists
     * `budget.json`. Must be called before [analyze]. If the process
     * dies, call again on restart — the key itself should live in
     * `EncryptedSharedPreferences`.
     */
    external fun configure(apiKey: String, budgetUsdDaily: Double, filesDir: String)

    /**
     * Submit a [com.companion.awareness.types.ContextEvent]-shaped JSON
     * and receive a [com.companion.awareness.types.FilterResponse]-shaped
     * JSON back. Runs the OpenAI filter call inside the core's tokio
     * runtime; blocks the caller thread, so invoke from a background
     * coroutine.
     */
    external fun analyze(eventJson: String): String

    /**
     * [analyze] plus an optional PNG screenshot of the active window.
     * `imagePng` may be null or empty — both behave exactly like
     * [analyze]. When present, the Rust core attaches the image to the
     * model call (gemma3:4b is vision-capable) so the model reasons over
     * the actual pixels — reels, photos, charts — instead of only the
     * extracted text. Same blocking semantics as [analyze].
     */
    external fun analyzeWithImage(eventJson: String, imagePng: ByteArray?): String

    /** Free-text biography the user can edit in ProfileActivity.
     *  Prepended to every system prompt from the next tick on. */
    external fun setBio(bio: String)

    /** Record a rating from a notification action. `positive=true` when
     *  the user tapped "mais disto"; the topic is appended to the
     *  profile's interests (or anti-interests on false). */
    external fun learnInterest(topic: String, positive: Boolean)

    /** Wipe everything the rating buttons accumulated (interests AND
     *  anti-interests). Explicit interests, bio and app usage stay.
     *  Escape hatch for a poisoned profile — junk rated "mais disto"
     *  persists and steers every later prompt, across app updates. */
    external fun clearLearnedInterests()

    /** JSON snapshot of the current profile — used by ProfileActivity
     *  to render bio + interests + anti-interests + top-apps summary. */
    external fun getProfileText(): String

    /** Replace the full list of user-curated explicit interests.
     *  Called whenever the user saves in ProfileActivity so the Rust
     *  matcher rebuilds immediately. */
    external fun setExplicitInterests(items: Array<String>)

    /** JSON array of the current explicit interests, for the UI to
     *  hydrate the pill list. */
    external fun getExplicitInterests(): String
}
