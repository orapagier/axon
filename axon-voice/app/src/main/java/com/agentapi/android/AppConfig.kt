package com.agentapi.android

import android.content.Context
import android.content.SharedPreferences
import java.util.UUID

/**
 * Single source of truth for all runtime configuration.
 * Stored in SharedPreferences — survives app restarts and updates.
 *
 * Settings are configurable from MainActivity or via:
 *   POST /config  { "webhookUrl": "...", "bearerToken": "..." }
 */
object AppConfig {

    private const val PREFS_NAME = "agentapi_config"

    private const val KEY_BEARER_TOKEN       = "bearer_token"       // legacy plaintext — migrated away from on read
    private const val KEY_BEARER_TOKEN_ENC   = "bearer_token_enc"
    private const val KEY_CLOUDFLARED_TOKEN_ENC = "cloudflared_token_enc"
    private const val KEY_WEBHOOK_URL    = "webhook_url"
    private const val KEY_PORT           = "port"
    private const val KEY_AUTO_ANSWER    = "auto_answer_enabled"
    private const val KEY_PUSH_SMS       = "push_sms"
    private const val KEY_PUSH_CALLS     = "push_calls"
    private const val KEY_PUSH_LOCATION  = "push_location"
    private const val KEY_PUSH_BATTERY   = "push_battery"
    private const val KEY_BATTERY_THRESHOLD = "battery_threshold"

    private fun prefs(ctx: Context): SharedPreferences =
        ctx.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    // ── Auth ──────────────────────────────────────────────────────────────────

    /**
     * Returns the bearer token, generating and persisting one on first run.
     * The user can replace this from MainActivity. Stored AES-GCM encrypted under
     * an AndroidKeyStore-resident key (see SecureStore) — a plaintext token left over
     * from before this migration is re-encrypted and cleared the first time it's read.
     */
    fun getBearerToken(ctx: Context): String {
        val p = prefs(ctx)
        p.getString(KEY_BEARER_TOKEN_ENC, null)?.let { enc -> SecureStore.decrypt(enc)?.let { return it } }

        // First run, or migrating a pre-encryption plaintext token
        val token = p.getString(KEY_BEARER_TOKEN, null)
            ?: UUID.randomUUID().toString().replace("-", "")
        setBearerToken(ctx, token)
        return token
    }

    fun setBearerToken(ctx: Context, token: String) =
        prefs(ctx).edit()
            .putString(KEY_BEARER_TOKEN_ENC, SecureStore.encrypt(token))
            .remove(KEY_BEARER_TOKEN)
            .apply()

    // ── Cloudflared tunnel token ─────────────────────────────────────────────
    // The connector token for the embedded, in-app cloudflared tunnel (see
    // CloudflaredManager). Created once in the Cloudflare Zero Trust dashboard;
    // changeable anytime from MainActivity or POST/PATCH /config.

    fun getCloudflaredToken(ctx: Context): String =
        prefs(ctx).getString(KEY_CLOUDFLARED_TOKEN_ENC, null)?.let { SecureStore.decrypt(it) } ?: ""

    fun setCloudflaredToken(ctx: Context, token: String) =
        prefs(ctx).edit().putString(KEY_CLOUDFLARED_TOKEN_ENC, SecureStore.encrypt(token)).apply()

    // ── Webhook ───────────────────────────────────────────────────────────────

    /** axon-agent webhook URL. e.g. https://your-axon-server.example.com/webhook/agentapi */
    fun getWebhookUrl(ctx: Context): String =
        prefs(ctx).getString(KEY_WEBHOOK_URL, "") ?: ""

    fun setWebhookUrl(ctx: Context, url: String) =
        prefs(ctx).edit().putString(KEY_WEBHOOK_URL, url).apply()

    // ── Server ────────────────────────────────────────────────────────────────

    fun getPort(ctx: Context): Int =
        prefs(ctx).getInt(KEY_PORT, 8080)

    fun setPort(ctx: Context, port: Int) =
        prefs(ctx).edit().putInt(KEY_PORT, port).apply()

    // ── Push event toggles ────────────────────────────────────────────────────

    fun isPushSmsEnabled(ctx: Context)      = prefs(ctx).getBoolean(KEY_PUSH_SMS, true)
    fun isPushCallsEnabled(ctx: Context)    = prefs(ctx).getBoolean(KEY_PUSH_CALLS, true)
    fun isPushLocationEnabled(ctx: Context) = prefs(ctx).getBoolean(KEY_PUSH_LOCATION, true)
    fun isPushBatteryEnabled(ctx: Context)  = prefs(ctx).getBoolean(KEY_PUSH_BATTERY, true)

    fun setPushSms(ctx: Context, v: Boolean)      = prefs(ctx).edit().putBoolean(KEY_PUSH_SMS, v).apply()
    fun setPushCalls(ctx: Context, v: Boolean)    = prefs(ctx).edit().putBoolean(KEY_PUSH_CALLS, v).apply()
    fun setPushLocation(ctx: Context, v: Boolean) = prefs(ctx).edit().putBoolean(KEY_PUSH_LOCATION, v).apply()
    fun setPushBattery(ctx: Context, v: Boolean)  = prefs(ctx).edit().putBoolean(KEY_PUSH_BATTERY, v).apply()

    // ── Auto-answer ───────────────────────────────────────────────────────────

    fun isAutoAnswerEnabled(ctx: Context) = prefs(ctx).getBoolean(KEY_AUTO_ANSWER, false)
    fun setAutoAnswer(ctx: Context, v: Boolean) =
        prefs(ctx).edit().putBoolean(KEY_AUTO_ANSWER, v).apply()

    // ── Battery push threshold ────────────────────────────────────────────────

    /** Push a battery event when level drops to or below this percentage. */
    fun getBatteryThreshold(ctx: Context): Int =
        prefs(ctx).getInt(KEY_BATTERY_THRESHOLD, 20)

    fun setBatteryThreshold(ctx: Context, pct: Int) =
        prefs(ctx).edit().putInt(KEY_BATTERY_THRESHOLD, pct).apply()
}
