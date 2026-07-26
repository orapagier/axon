package com.agentapi.android

import android.content.Context
import android.util.Log
import com.google.gson.Gson
import io.ktor.client.*
import io.ktor.client.engine.okhttp.*
import io.ktor.client.plugins.contentnegotiation.*
import io.ktor.client.request.*
import io.ktor.client.statement.*
import io.ktor.http.*
import io.ktor.serialization.gson.*
import kotlinx.coroutines.*

/**
 * Pushes structured events to the configured axon-agent webhook URL.
 *
 * This is the core of the "phone as event emitter" architecture.
 * Instead of axon-agent polling the phone, the phone proactively fires events.
 * axon-agent receives them via its own HTTP ingest endpoint.
 *
 * Event envelope:
 * {
 *   "event":     "sms_received" | "call_missed" | "call_incoming" |
 *                "location_changed" | "battery_low",
 *   "timestamp": 1712345678000,
 *   "data":      { ...event-specific fields... }
 * }
 */
object WebhookPusher {

    private const val TAG = "WebhookPusher"

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val gson  = Gson()

    private val httpClient = HttpClient(OkHttp) {
        install(ContentNegotiation) { gson() }
        engine {
            config {
                connectTimeout(10, java.util.concurrent.TimeUnit.SECONDS)
                readTimeout(15, java.util.concurrent.TimeUnit.SECONDS)
            }
        }
    }

    // ── Public push methods ───────────────────────────────────────────────────

    fun pushSms(ctx: Context, from: String, body: String, timestamp: Long) {
        if (!AppConfig.isPushSmsEnabled(ctx)) return
        push(ctx, "sms_received", mapOf(
            "from"      to from,
            "body"      to body,
            "timestamp" to timestamp
        ))
    }

    fun pushCall(ctx: Context, event: String, number: String, name: String?) {
        if (!AppConfig.isPushCallsEnabled(ctx)) return
        push(ctx, event, mapOf(
            "number" to number,
            "name"   to (name ?: "Unknown"),
        ))
    }

    fun pushLocation(ctx: Context, lat: Double, lon: Double, accuracy: Float, provider: String) {
        if (!AppConfig.isPushLocationEnabled(ctx)) return
        push(ctx, "location_changed", mapOf(
            "latitude"        to lat,
            "longitude"       to lon,
            "accuracy_meters" to accuracy,
            "provider"        to provider,
            "maps_url"        to "https://www.google.com/maps?q=$lat,$lon"
        ))
    }

    fun pushBattery(ctx: Context, percent: Int, charging: Boolean, plugged: String) {
        if (!AppConfig.isPushBatteryEnabled(ctx)) return
        push(ctx, "battery_low", mapOf(
            "percent"  to percent,
            "charging" to charging,
            "plugged"  to plugged
        ))
    }

    // ── Core push (fire-and-forget, retries once on failure) ──────────────────

    private fun push(ctx: Context, event: String, data: Map<String, Any?>) {
        val url = AppConfig.getWebhookUrl(ctx)
        if (url.isBlank()) {
            Log.d(TAG, "No webhook URL configured — skipping push for '$event'")
            return
        }

        val payload = mapOf(
            "event"     to event,
            "timestamp" to System.currentTimeMillis(),
            "data"      to data
        )

        scope.launch {
            repeat(2) { attempt ->
                try {
                    val response: HttpResponse = httpClient.post(url) {
                        contentType(ContentType.Application.Json)
                        setBody(gson.toJson(payload))
                        header("X-AgentAPI-Event", event)
                    }
                    Log.i(TAG, "Pushed '$event' → HTTP ${response.status.value}")
                    return@launch
                } catch (e: Exception) {
                    Log.w(TAG, "Push attempt ${attempt + 1} failed for '$event': ${e.message}")
                    if (attempt == 0) delay(3_000)
                }
            }
        }
    }

    fun shutdown() {
        scope.cancel()
        httpClient.close()
    }
}
