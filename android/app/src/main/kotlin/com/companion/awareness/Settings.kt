package com.companion.awareness

import android.content.Context
import android.content.SharedPreferences
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey

/**
 * Persisted user settings. The OpenAI API key lives in
 * EncryptedSharedPreferences (AES256 under a hardware-backed master
 * key when the device supports it). If the encrypted store cannot be
 * initialised — happens on some emulators / broken Keystores — we log
 * and fall back to plain SharedPreferences so the app keeps working,
 * but this should NOT happen on a real device.
 */
object Settings {
    private const val FILE_SECURE = "awareness-secure"
    private const val FILE_PLAIN = "awareness-settings"
    private const val KEY_OPENAI = "openai_api_key"
    private const val KEY_BUDGET_USD = "budget_usd_daily"
    private const val KEY_TTS_ENABLED = "tts_enabled"
    private const val KEY_USER_BIO = "user_bio"
    // Dev/test default: kept generous so the app doesn't pause alerts
    // mid-session during active development. Tighten before shipping.
    private const val DEFAULT_BUDGET_USD = 5.0f
    private const val DEFAULT_TTS_ENABLED = true

    // Encrypted store holds the API key. Non-sensitive flags (budget,
    // tts toggle) live in the plain store so they don't silently
    // disappear if the Keystore gets reset.
    private fun securePrefs(ctx: Context): SharedPreferences = try {
        val master = MasterKey.Builder(ctx)
            .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
            .build()
        EncryptedSharedPreferences.create(
            ctx,
            FILE_SECURE,
            master,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
    } catch (t: Throwable) {
        AppLog.e("Settings", "EncryptedSharedPreferences failed; falling back to plain", t)
        ctx.getSharedPreferences(FILE_PLAIN, Context.MODE_PRIVATE)
    }

    private fun plainPrefs(ctx: Context): SharedPreferences =
        ctx.getSharedPreferences(FILE_PLAIN, Context.MODE_PRIVATE)

    fun openAiKey(ctx: Context): String =
        securePrefs(ctx).getString(KEY_OPENAI, "") ?: ""

    fun setOpenAiKey(ctx: Context, value: String) {
        securePrefs(ctx).edit().putString(KEY_OPENAI, value.trim()).apply()
    }

    fun budgetUsdDaily(ctx: Context): Double =
        plainPrefs(ctx).getFloat(KEY_BUDGET_USD, DEFAULT_BUDGET_USD).toDouble()

    fun ttsEnabled(ctx: Context): Boolean =
        plainPrefs(ctx).getBoolean(KEY_TTS_ENABLED, DEFAULT_TTS_ENABLED)

    fun setTtsEnabled(ctx: Context, value: Boolean) {
        plainPrefs(ctx).edit().putBoolean(KEY_TTS_ENABLED, value).apply()
    }

    fun userBio(ctx: Context): String =
        plainPrefs(ctx).getString(KEY_USER_BIO, "") ?: ""

    fun setUserBio(ctx: Context, value: String) {
        plainPrefs(ctx).edit().putString(KEY_USER_BIO, value).apply()
    }

    private const val KEY_EXPLICIT_INTERESTS = "explicit_interests_json"

    /** Loads the user's curated interests from disk. Stored as a JSON
     *  array of strings so ordering + non-ASCII labels survive. */
    fun explicitInterests(ctx: Context): List<String> {
        val raw = plainPrefs(ctx).getString(KEY_EXPLICIT_INTERESTS, "[]") ?: "[]"
        return runCatching {
            val arr = org.json.JSONArray(raw)
            (0 until arr.length()).mapNotNull { idx ->
                arr.optString(idx, "").takeIf { it.isNotBlank() }
            }
        }.getOrDefault(emptyList())
    }

    fun setExplicitInterests(ctx: Context, values: List<String>) {
        val arr = org.json.JSONArray()
        values.forEach { arr.put(it) }
        plainPrefs(ctx).edit().putString(KEY_EXPLICIT_INTERESTS, arr.toString()).apply()
    }
}
