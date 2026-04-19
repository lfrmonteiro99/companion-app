package com.companion.awareness

import android.content.Context

/**
 * Thin wrapper around SharedPreferences for the OpenAI API key and any
 * other simple settings. A single app install owns its own key — we do
 * not share with the desktop build (that's a future auth feature).
 *
 * TODO: swap to `EncryptedSharedPreferences` from `androidx.security:security-crypto`
 * before this ships to real users. For now plain SharedPreferences keeps the
 * scaffold build-dep-light.
 */
object Settings {
    private const val PREFS = "awareness-settings"
    private const val KEY_OPENAI = "openai_api_key"
    private const val KEY_BUDGET_USD = "budget_usd_daily"
    private const val DEFAULT_BUDGET_USD = 0.5f

    fun openAiKey(ctx: Context): String =
        ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .getString(KEY_OPENAI, "") ?: ""

    fun setOpenAiKey(ctx: Context, value: String) {
        ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit()
            .putString(KEY_OPENAI, value.trim())
            .apply()
    }

    fun budgetUsdDaily(ctx: Context): Double =
        ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .getFloat(KEY_BUDGET_USD, DEFAULT_BUDGET_USD)
            .toDouble()
}
