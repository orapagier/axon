package com.agentapi.android

import com.axon.voice.R
import android.annotation.SuppressLint
import android.Manifest
import android.provider.AlarmClock
import android.app.AlarmManager
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.database.Cursor
import android.graphics.BitmapFactory
import android.graphics.ImageFormat
import android.hardware.camera2.*
import android.hardware.camera2.params.OutputConfiguration
import android.hardware.camera2.params.SessionConfiguration
import android.media.AudioManager
import android.media.Image
import android.media.ImageReader
import android.media.MediaPlayer
import android.net.Uri
import android.net.wifi.WifiManager
import android.os.BatteryManager
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.os.StatFs
import android.provider.CallLog
import android.provider.ContactsContract
import android.provider.Telephony
import android.speech.tts.TextToSpeech
import android.telecom.TelecomManager
import android.telephony.SmsManager
import android.util.Base64
import android.util.Log
import android.util.Size
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import com.google.gson.Gson
import io.ktor.http.*
import io.ktor.serialization.gson.*
import io.ktor.server.application.*
import io.ktor.server.auth.*
import io.ktor.server.auth.bearer
import io.ktor.server.engine.*
import io.ktor.server.netty.*
import io.ktor.server.plugins.contentnegotiation.*
import io.ktor.server.plugins.statuspages.*
import io.ktor.server.request.*
import io.ktor.server.response.*
import io.ktor.server.routing.*
import kotlinx.coroutines.*
import java.io.File
import java.net.NetworkInterface
import java.nio.ByteBuffer
import java.security.MessageDigest
import java.util.*
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
import java.util.zip.ZipEntry
import java.util.zip.ZipOutputStream

class ApiServer(private val context: Context) {

    private val TAG  = "ApiServer"
    private val gson = Gson()

    private var engine: NettyApplicationEngine? = null
    private var tts: TextToSpeech? = null

    companion object {
        /**
         * Detects Termux using two methods in order:
         * 1. PackageManager.getPackageInfo — works when <queries><package android:name="com.termux"/>
         *    is declared in AndroidManifest.xml (required on Android 11+).
         * 2. Fallback: attempt to resolve Termux's RunCommandService via Intent — works even
         *    without the <queries> declaration because startForegroundService() is not
         *    blocked by package visibility restrictions.
         *
         * The original File.exists() approach is intentionally NOT used here because
         * /data/data/com.termux/ is not readable from another app's sandbox (SELinux).
         *
         * FIX for "termux_available: false" on Android 11+:
         * Add to AndroidManifest.xml:  <queries><package android:name="com.termux"/></queries>
         */
        fun isTermuxAvailable(context: Context): Boolean {
            // Method 1: PackageManager (fast, accurate when <queries> is declared)
            try {
                context.packageManager.getPackageInfo("com.termux", 0)
                return true
            } catch (_: Exception) {}
            // Method 2: Intent resolution fallback (works even without <queries> on Android 11+)
            // If Termux's RunCommandService can be resolved, Termux is installed.
            try {
                val intent = android.content.Intent().apply {
                    setClassName("com.termux", "com.termux.app.RunCommandService")
                }
                val info = context.packageManager.resolveService(intent, 0)
                if (info != null) return true
            } catch (_: Exception) {}
            // Method 3: Check if the Termux RUN_COMMAND permission is declared (last resort)
            try {
                context.packageManager.getPermissionInfo("com.termux.permission.RUN_COMMAND", 0)
                return true
            } catch (_: Exception) {}
            return false
        }

        /** Kept for status/info display — the path itself cannot be exec'd directly. */
        fun findTermuxBash(): String? =
            listOf(
                "/data/data/com.termux/files/usr/bin/bash",
                "/data/user/0/com.termux/files/usr/bin/bash"
            ).firstOrNull { File(it).exists() }
                ?: "/data/data/com.termux/files/usr/bin/bash" // assumed default

        fun getLocalIp(): String = try {
            NetworkInterface.getNetworkInterfaces()
                .asSequence()
                .filter { it.isUp && !it.isLoopback }
                .flatMap { it.inetAddresses.asSequence() }
                .filterIsInstance<java.net.Inet4Address>()
                .firstOrNull()?.hostAddress ?: "127.0.0.1"
        } catch (e: Exception) { "127.0.0.1" }

        private const val NOTIF_CHANNEL_ID = "agent_api_channel"
    }

    // ── File sandbox ─────────────────────────────────────────────────────────
    // Default safe root for writes/deletes. Paths resolving outside it aren't blocked
    // outright (you may genuinely want to manage files elsewhere) — they just require an
    // on-device approval tap first, same as shell.run and other irreversible actions.
    private val sandboxRoot: File by lazy {
        File(android.os.Environment.getExternalStorageDirectory(), "AgentAPI").apply { mkdirs() }
    }

    private fun isInsideSandbox(file: File): Boolean = try {
        val canonical = file.canonicalFile
        val root      = sandboxRoot.canonicalFile
        canonical == root || canonical.path.startsWith(root.path + File.separator)
    } catch (e: Exception) {
        false
    }

    /** Requires on-device approval when [file] falls outside the sandbox root. True = proceed. */
    private suspend fun requireApprovalIfOutsideSandbox(
        call: ApplicationCall, file: File, verb: String
    ): Boolean {
        if (isInsideSandbox(file)) return true
        return requireApproval(call, "$verb outside the AgentAPI sandbox:\n${file.absolutePath}")
    }

    /** Pushes an on-device Approve/Deny notification and suspends for a decision. True = approved. */
    private suspend fun requireApproval(call: ApplicationCall, description: String): Boolean {
        val approved = ApprovalManager.requestApproval(context, description)
        AuditLog.record(context, "approval", description, if (approved) "approved" else "denied_or_timeout")
        if (!approved) {
            call.respond(HttpStatusCode.Forbidden, mapOf(
                "error"             to "Action requires on-device approval — it was denied or no one responded in time.",
                "requires_approval" to true
            ))
        }
        return approved
    }

    // ── Start / Stop ──────────────────────────────────────────────────────────

    fun start() {
        val ctx: Context = context

        // Initialise TTS engine once at startup
        tts = TextToSpeech(ctx) { status ->
            if (status == TextToSpeech.SUCCESS) {
                tts?.language = Locale.getDefault()
                Log.i(TAG, "TTS engine ready")
            } else {
                Log.w(TAG, "TTS init failed with status $status")
            }
        }

        // Create notification channel (required for Android 8+)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(
                NOTIF_CHANNEL_ID,
                "Agent API Notifications",
                NotificationManager.IMPORTANCE_DEFAULT
            )
            val nm = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
            nm.createNotificationChannel(channel)
        }

        val token = AppConfig.getBearerToken(ctx)
        val port  = AppConfig.getPort(ctx)

        // Loopback-only: the only intended entry points are (a) this device's own processes
        // (the embedded cloudflared tunnel — see CloudflaredManager) and (b) direct localhost
        // testing. Binding 0.0.0.0 previously made the server reachable to anyone on the same
        // Wi-Fi/LAN over plaintext HTTP, bypassing the tunnel entirely. No CORS plugin either —
        // every real caller (axon-agent, curl, cloudflared) is a server-to-server client, not a browser.
        engine = embeddedServer(Netty, port = port, host = "127.0.0.1") {

            // ── Plugins ───────────────────────────────────────────────────────

            install(ContentNegotiation) { gson() }

            install(StatusPages) {
                exception<Throwable> { call, cause ->
                    Log.e(TAG, "Unhandled error: ${cause.message}", cause)
                    call.respond(HttpStatusCode.InternalServerError,
                        mapOf("error" to (cause.message ?: "Internal error")))
                }
            }

            install(Authentication) {
                bearer("auth-bearer") {
                    authenticate { credential ->
                        // Constant-time compare — plain `==` leaks a timing side-channel on the token.
                        val provided = credential.token.toByteArray(Charsets.UTF_8)
                        val expected = token.toByteArray(Charsets.UTF_8)
                        if (MessageDigest.isEqual(provided, expected)) UserIdPrincipal("agent") else null
                    }
                }
            }

            // ── Routes ────────────────────────────────────────────────────────

            routing {

                // Health check — no auth
                get("/ping") {
                    call.respond(mapOf("ok" to true, "ts" to System.currentTimeMillis()))
                }

                authenticate("auth-bearer") {

                    // ── Status / discovery ────────────────────────────────────
                    get("/status") {
                        call.respond(mapOf(
                            "status"          to "running",
                            "version"         to "4.0",
                            "termux"          to isTermuxAvailable(ctx),
                            "termux_path"     to (findTermuxBash() ?: "not found"),
                            "local_ip"        to getLocalIp(),
                            "bind"            to "127.0.0.1 (loopback only — external access is via the Cloudflare Tunnel)",
                            "cloudflared_running" to CloudflaredManager.isRunning(),
                            "file_sandbox"    to sandboxRoot.absolutePath,
                            "sandbox_note"    to "PUT/DELETE file ops and shell.run outside this folder require an on-device approval tap",
                            "endpoints"   to listOf(
                                // ── GET ──
                                "GET  /ping                    — health check (no auth)",
                                "GET  /status                  — server info + endpoint list",
                                "GET  /config                  — view config",
                                "GET  /sms/inbox               — read SMS (limit, offset)",
                                "GET  /call/log                — call history (limit)",
                                "GET  /contacts                — list/search contacts (q, limit)",
                                "GET  /battery                 — battery status",
                                "GET  /device                  — device info",
                                "GET  /device/volume           — current volume for all streams",
                                "GET  /device/brightness       — current screen brightness (0-255)",
                                "GET  /device/storage          — internal/external storage info",
                                "GET  /device/apps             — list installed apps",
                                "GET  /device/clipboard        — read clipboard text",
                                "GET  /device/ringermode       — current ringer mode",
                                "GET  /device/audio            — whether audio is currently playing",
                                "GET  /wifi/info               — WiFi SSID, signal, IP",
                                "GET  /location                — current location",
                                "GET  /camera/photo            — take photo (camera: back|front)",
                                "GET  /files                   — browse files (path)",
                                "GET  /files/download          — download file (path)",
                                // ── POST ──
                                "POST /config                  — update config (full body)",
                                "POST /sms/send                — send SMS (number, message)",
                                "POST /call                    — initiate call (number)",
                                "POST /shell                   — run shell command (command, termux: bool)",
                                "POST /notification            — push local notification (title, message, id?)",
                                "POST /device/tts              — text-to-speech (text, language?)",
                                "POST /device/vibrate          — vibrate (pattern: [ms,ms,...])",
                                "POST /device/flashlight       — toggle flashlight (enabled: bool)",
                                "POST /device/clipboard        — set clipboard (text)",
                                "POST /device/toast            — show toast message (text)",
                                "POST /launch/app              — launch app (package)",
                                "POST /launch/url              — open URL in browser (url)",
                                "POST /alarm                   — set alarm via AlarmManager (hour, minute, label?)",
                                "GET  /alarm/status            — check alarm permission + next scheduled alarm",
                                "DELETE /alarm                 — cancel alarm (?hour=, ?minute=)",
                                "POST /media/play              — play audio file (path)",
                                "POST /media/stop              — stop current media playback",
                                "POST /files/zip               — zip files (sources: [path], dest: path)",
                                // ── PATCH ──
                                "PATCH /config                 — update config (partial body)",
                                "PATCH /device/volume          — set volume (stream, level)",
                                "PATCH /device/brightness      — set brightness (level: 0-255)",
                                "PATCH /device/ringermode      — set ringer (mode: silent|vibrate|normal)",
                                // ── PUT ──
                                "PUT  /files                   — upload/overwrite file (?path=, body=bytes)",
                                "PUT  /device/wallpaper        — set wallpaper (body=jpeg bytes)",
                                // ── DELETE ──
                                "DELETE /files                 — delete file or directory (?path=)",
                                "DELETE /contacts/{id}         — delete contact by id",
                                "DELETE /call/log              — clear entire call log",
                                // ── AGENT ──
                                "GET  /agent/tools             — list all available tools for LLM",
                                "POST /agent/tool              — execute any tool by name (tool, params)",
                                // ── AUDIT ──
                                "GET  /audit/log               — recent sensitive-action history (?lines=)"
                            )
                        ))
                    }

                    // ── Config ────────────────────────────────────────────────
                    get("/config") {
                        call.respond(mapOf(
                            "webhook_url"       to AppConfig.getWebhookUrl(ctx),
                            "port"              to AppConfig.getPort(ctx),
                            "auto_answer"       to AppConfig.isAutoAnswerEnabled(ctx),
                            "push_sms"          to AppConfig.isPushSmsEnabled(ctx),
                            "push_calls"        to AppConfig.isPushCallsEnabled(ctx),
                            "push_location"     to AppConfig.isPushLocationEnabled(ctx),
                            "push_battery"      to AppConfig.isPushBatteryEnabled(ctx),
                            "battery_threshold" to AppConfig.getBatteryThreshold(ctx),
                            "cloudflared_token_set" to AppConfig.getCloudflaredToken(ctx).isNotBlank(),
                            "note"              to "bearer_token / cloudflared_token values not shown for security"
                        ))
                    }

                    post("/config") {
                        val body = call.receive<Map<String, Any>>()
                        body["webhook_url"]?.toString()?.let { AppConfig.setWebhookUrl(ctx, it) }
                        body["bearer_token"]?.toString()?.let { AppConfig.setBearerToken(ctx, it) }
                        body["cloudflared_token"]?.toString()?.let {
                            AppConfig.setCloudflaredToken(ctx, it)
                            CloudflaredManager.start(ctx)
                        }
                        body["auto_answer"]?.let {
                            AppConfig.setAutoAnswer(ctx, it.toString().toBooleanStrictOrNull() ?: false)
                        }
                        body["push_sms"]?.let {
                            AppConfig.setPushSms(ctx, it.toString().toBooleanStrictOrNull() ?: true)
                        }
                        body["push_calls"]?.let {
                            AppConfig.setPushCalls(ctx, it.toString().toBooleanStrictOrNull() ?: true)
                        }
                        body["push_location"]?.let {
                            AppConfig.setPushLocation(ctx, it.toString().toBooleanStrictOrNull() ?: true)
                        }
                        body["push_battery"]?.let {
                            AppConfig.setPushBattery(ctx, it.toString().toBooleanStrictOrNull() ?: true)
                        }
                        (body["battery_threshold"] as? Double)?.let {
                            AppConfig.setBatteryThreshold(ctx, it.toInt())
                        }
                        AuditLog.record(ctx, "config.update", body.keys.joinToString(","), "ok")
                        call.respond(mapOf("ok" to true))
                    }

                    patch("/config") {
                        val body = call.receive<Map<String, Any>>()
                        body["webhook_url"]?.toString()?.let { AppConfig.setWebhookUrl(ctx, it) }
                        body["bearer_token"]?.toString()?.let { AppConfig.setBearerToken(ctx, it) }
                        body["cloudflared_token"]?.toString()?.let {
                            AppConfig.setCloudflaredToken(ctx, it)
                            CloudflaredManager.start(ctx)
                        }
                        body["auto_answer"]?.let {
                            AppConfig.setAutoAnswer(ctx, it.toString().toBooleanStrictOrNull() ?: false)
                        }
                        body["push_sms"]?.let {
                            AppConfig.setPushSms(ctx, it.toString().toBooleanStrictOrNull() ?: true)
                        }
                        body["push_calls"]?.let {
                            AppConfig.setPushCalls(ctx, it.toString().toBooleanStrictOrNull() ?: true)
                        }
                        body["push_location"]?.let {
                            AppConfig.setPushLocation(ctx, it.toString().toBooleanStrictOrNull() ?: true)
                        }
                        body["push_battery"]?.let {
                            AppConfig.setPushBattery(ctx, it.toString().toBooleanStrictOrNull() ?: true)
                        }
                        AuditLog.record(ctx, "config.update", body.keys.joinToString(","), "ok")
                        call.respond(mapOf("ok" to true, "updated_fields" to body.keys))
                    }

                    // ── Audit log ─────────────────────────────────────────────
                    get("/audit/log") {
                        val lines = call.request.queryParameters["lines"]?.toIntOrNull() ?: 200
                        call.respond(mapOf("entries" to AuditLog.tail(ctx, lines)))
                    }

                    // ── SMS ───────────────────────────────────────────────────
                    post("/sms/send") {
                        if (!checkPermission(call, Manifest.permission.SEND_SMS)) return@post
                        val body    = call.receive<Map<String, String>>()
                        val number  = body["number"]  ?: error("Missing 'number'")
                        val message = body["message"] ?: error("Missing 'message'")
                        sendSms(number, message)
                        call.respond(mapOf("ok" to true, "to" to number))
                    }

                    get("/sms/inbox") {
                        if (!checkPermission(call, Manifest.permission.READ_SMS)) return@get
                        val limit  = call.request.queryParameters["limit"]?.toIntOrNull()  ?: 20
                        val offset = call.request.queryParameters["offset"]?.toIntOrNull() ?: 0
                        call.respond(mapOf("messages" to getSmsInbox(limit, offset)))
                    }

                    // ── Calls ─────────────────────────────────────────────────
                    post("/call") {
                        if (!checkPermission(call, Manifest.permission.CALL_PHONE)) return@post
                        val body   = call.receive<Map<String, String>>()
                        val number = body["number"] ?: error("Missing 'number'")
                        initiateCall(number)
                        call.respond(mapOf("ok" to true, "calling" to number))
                    }

                    get("/call/log") {
                        if (!checkPermission(call, Manifest.permission.READ_CALL_LOG)) return@get
                        val limit = call.request.queryParameters["limit"]?.toIntOrNull() ?: 20
                        call.respond(mapOf("calls" to getCallLog(limit)))
                    }

                    delete("/call/log") {
                        if (!checkPermission(call, Manifest.permission.WRITE_CALL_LOG)) return@delete
                        if (!requireApproval(call, "Clear the entire call log")) return@delete
                        val deleted = clearCallLog()
                        AuditLog.record(ctx, "call.log.delete", "deleted_count=$deleted", "ok")
                        call.respond(mapOf("ok" to true, "deleted_count" to deleted))
                    }

                    // ── Contacts ──────────────────────────────────────────────
                    get("/contacts") {
                        if (!checkPermission(call, Manifest.permission.READ_CONTACTS)) return@get
                        val query = call.request.queryParameters["q"]
                        val limit = call.request.queryParameters["limit"]?.toIntOrNull() ?: 50
                        call.respond(mapOf("contacts" to getContacts(query, limit)))
                    }

                    delete("/contacts/{id}") {
                        if (!checkPermission(call, Manifest.permission.WRITE_CONTACTS)) return@delete
                        val id = call.parameters["id"] ?: error("Missing contact id")
                        if (!requireApproval(call, "Delete contact id=$id")) return@delete
                        val deleted = deleteContact(id)
                        AuditLog.record(ctx, "contacts.delete", "id=$id", if (deleted) "ok" else "failed")
                        call.respond(mapOf("ok" to deleted, "id" to id))
                    }

                    // ── Battery ───────────────────────────────────────────────
                    get("/battery") {
                        call.respond(getBattery())
                    }

                    // ── Device info ───────────────────────────────────────────
                    get("/device") {
                        call.respond(getDevice())
                    }

                    // ── Volume ────────────────────────────────────────────────
                    get("/device/volume") {
                        call.respond(getVolume(ctx))
                    }

                    patch("/device/volume") {
                        val body   = call.receive<Map<String, Any>>()
                        val level  = (body["level"] as? Double)?.toInt()
                            ?: error("Missing 'level'")
                        val stream = body["stream"]?.toString() ?: "music"
                        setVolume(ctx, stream, level)
                        call.respond(mapOf("ok" to true, "stream" to stream, "volume" to level))
                    }

                    // ── Brightness ────────────────────────────────────────────
                    // Requires WRITE_SETTINGS — grant once via Settings > Apps >
                    // Special app access > Modify system settings
                    get("/device/brightness") {
                        val level = android.provider.Settings.System.getInt(
                            ctx.contentResolver,
                            android.provider.Settings.System.SCREEN_BRIGHTNESS, -1
                        )
                        call.respond(mapOf("brightness" to level, "max" to 255))
                    }

                    patch("/device/brightness") {
                        val body  = call.receive<Map<String, Any>>()
                        val level = (body["level"] as? Double)?.toInt()
                            ?: error("Missing 'level' (0–255)")
                        setBrightness(ctx, level)
                        call.respond(mapOf("ok" to true, "brightness" to level))
                    }

                    // ── Ringer mode ───────────────────────────────────────────
                    get("/device/ringermode") {
                        val am   = ctx.getSystemService(Context.AUDIO_SERVICE) as AudioManager
                        val mode = when (am.ringerMode) {
                            AudioManager.RINGER_MODE_SILENT  -> "silent"
                            AudioManager.RINGER_MODE_VIBRATE -> "vibrate"
                            else                             -> "normal"
                        }
                        call.respond(mapOf("ringer_mode" to mode))
                    }

                    patch("/device/ringermode") {
                        val body = call.receive<Map<String, Any>>()
                        val mode = body["mode"]?.toString() ?: error("Missing 'mode' (silent|vibrate|normal)")
                        setRingerMode(ctx, mode)
                        call.respond(mapOf("ok" to true, "ringer_mode" to mode))
                    }

                    // ── Storage ───────────────────────────────────────────────
                    get("/device/storage") {
                        call.respond(getStorage())
                    }

                    // ── Installed apps ────────────────────────────────────────
                    get("/device/apps") {
                        val pm   = ctx.packageManager
                        val apps = pm.getInstalledApplications(PackageManager.GET_META_DATA)
                            .filter { pm.getLaunchIntentForPackage(it.packageName) != null }
                            .map { app -> mapOf(
                                "name"    to pm.getApplicationLabel(app).toString(),
                                "package" to app.packageName
                            )}
                            .sortedBy { it["name"] as String }
                        call.respond(mapOf("count" to apps.size, "apps" to apps))
                    }

                    // ── Clipboard ─────────────────────────────────────────────
                    get("/device/clipboard") {
                        val cm   = ctx.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                        val text = cm.primaryClip?.getItemAt(0)?.text?.toString()
                        call.respond(mapOf("text" to text))
                    }

                    post("/device/clipboard") {
                        val body = call.receive<Map<String, String>>()
                        val text = body["text"] ?: error("Missing 'text'")
                        val cm   = ctx.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                        cm.setPrimaryClip(ClipData.newPlainText("agent", text))
                        call.respond(mapOf("ok" to true))
                    }

                    // ── Audio playing status ───────────────────────────────────
                    get("/device/audio") {
                        val am      = ctx.getSystemService(Context.AUDIO_SERVICE) as AudioManager
                        val playing = am.isMusicActive
                        call.respond(mapOf("playing" to playing))
                    }

                    // ── WiFi info ─────────────────────────────────────────────
                    get("/wifi/info") {
                        call.respond(getWifiInfo(ctx))
                    }

                    // ── Notifications ─────────────────────────────────────────
                    post("/notification") {
                        val body    = call.receive<Map<String, Any>>()
                        val title   = body["title"]?.toString()   ?: error("Missing 'title'")
                        val message = body["message"]?.toString() ?: error("Missing 'message'")
                        val id      = (body["id"] as? Double)?.toInt() ?: (System.currentTimeMillis() % Int.MAX_VALUE).toInt()
                        pushNotification(ctx, title, message, id)
                        call.respond(mapOf("ok" to true, "id" to id))
                    }

                    // ── TTS ───────────────────────────────────────────────────
                    post("/device/tts") {
                        val body = call.receive<Map<String, Any>>()
                        val text = body["text"]?.toString() ?: error("Missing 'text'")
                        val lang = body["language"]?.toString()
                        speakText(text, lang)
                        call.respond(mapOf("ok" to true, "text" to text))
                    }

                    // ── Vibrate ───────────────────────────────────────────────
                    post("/device/vibrate") {
                        val body    = call.receive<Map<String, Any>>()
                        @Suppress("UNCHECKED_CAST")
                        val pattern = (body["pattern"] as? List<Double>)
                            ?.map { it.toLong() }?.toLongArray()
                            ?: longArrayOf(0, 300)
                        vibrateDevice(ctx, pattern)
                        call.respond(mapOf("ok" to true))
                    }

                    // ── Flashlight ────────────────────────────────────────────
                    post("/device/flashlight") {
                        val body    = call.receive<Map<String, Any>>()
                        val enabled = body["enabled"]?.toString()?.toBooleanStrictOrNull()
                            ?: error("Missing 'enabled' boolean")
                        setFlashlight(ctx, enabled)
                        call.respond(mapOf("ok" to true, "flashlight" to enabled))
                    }

                    // ── Toast ─────────────────────────────────────────────────
                    post("/device/toast") {
                        val body = call.receive<Map<String, Any>>()
                        val text = body["text"]?.toString() ?: error("Missing 'text'")
                        // Toast must run on the main thread
                        Handler(ctx.mainLooper).post {
                            android.widget.Toast.makeText(ctx, text, android.widget.Toast.LENGTH_LONG).show()
                        }
                        call.respond(mapOf("ok" to true))
                    }

                    // ── Launch app ────────────────────────────────────────────
                    post("/launch/app") {
                        val body    = call.receive<Map<String, Any>>()
                        val pkg     = body["package"]?.toString() ?: error("Missing 'package'")
                        val intent  = ctx.packageManager.getLaunchIntentForPackage(pkg)
                            ?: error("App not found or not launchable: $pkg")
                        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                        ctx.startActivity(intent)
                        call.respond(mapOf("ok" to true, "package" to pkg))
                    }

                    // ── Launch URL ────────────────────────────────────────────
                    post("/launch/url") {
                        val body   = call.receive<Map<String, Any>>()
                        val url    = body["url"]?.toString() ?: error("Missing 'url'")
                        val intent = Intent(Intent.ACTION_VIEW, Uri.parse(url))
                            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                        ctx.startActivity(intent)
                        call.respond(mapOf("ok" to true, "url" to url))
                    }

                    // ── Alarm ─────────────────────────────────────────────────

                    // Diagnostic: check if alarm permission is granted before trying to set one.
                    // axon-agent should call this first; if can_schedule_exact_alarms=false, tell the user.
                    get("/alarm/status") {
                        val am = ctx.getSystemService(Context.ALARM_SERVICE) as AlarmManager
                        val canSchedule = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                            am.canScheduleExactAlarms()
                        } else { true }
                        val nextAlarm = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.LOLLIPOP) {
                            am.nextAlarmClock?.let {
                                mapOf("trigger_time" to java.util.Date(it.triggerTime).toString(),
                                    "trigger_ms" to it.triggerTime)
                            }
                        } else { null }
                        call.respond(mapOf(
                            "can_schedule_exact_alarms" to canSchedule,
                            "next_alarm" to nextAlarm,
                            "setup_required" to !canSchedule,
                            "setup_instructions" to if (!canSchedule)
                                "On the phone: Settings → Apps → AgentAPI → Alarms & reminders → Allow"
                            else "OK — alarm permission granted"
                        ))
                    }

                    post("/alarm") {
                        val body   = call.receive<Map<String, Any>>()
                        val hour   = body["hour"]?.toString()?.toDouble()?.toInt() ?: error("Missing 'hour' (0-23)")
                        val minute = body["minute"]?.toString()?.toDouble()?.toInt() ?: error("Missing 'minute' (0-59)")
                        val label  = body["label"]?.toString() ?: "Agent Alarm"
                        setAlarm(ctx, hour, minute, label)
                        call.respond(mapOf(
                            "ok"    to true,
                            "hour"  to hour,
                            "minute" to minute,
                            "label" to label,
                            "time"  to "${hour.toString().padStart(2,'0')}:${minute.toString().padStart(2,'0')}"
                        ))
                    }

                    // Cancel alarm — DELETE /alarm?hour=8&minute=30
                    delete("/alarm") {
                        val hour   = call.request.queryParameters["hour"]?.toIntOrNull()
                            ?: error("Missing 'hour' query param")
                        val minute = call.request.queryParameters["minute"]?.toIntOrNull()
                            ?: error("Missing 'minute' query param")
                        cancelAlarm(ctx, hour, minute)
                        call.respond(mapOf(
                            "ok"        to true,
                            "cancelled" to "${hour.toString().padStart(2,'0')}:${minute.toString().padStart(2,'0')}"
                        ))
                    }
                    // ── Media ─────────────────────────────────────────────────
                    post("/media/play") {
                        val body = call.receive<Map<String, Any>>()
                        val path = body["path"]?.toString() ?: error("Missing 'path'")
                        playMedia(path)
                        call.respond(mapOf("ok" to true, "path" to path))
                    }

                    post("/media/stop") {
                        stopMedia()
                        call.respond(mapOf("ok" to true))
                    }

                    // ── Location ──────────────────────────────────────────────
                    get("/location") {
                        call.respond(getLocation())
                    }

                    // ── Camera ────────────────────────────────────────────────
                    get("/camera/photo") {
                        if (!checkPermission(call, Manifest.permission.CAMERA)) return@get
                        val facing = call.request.queryParameters["camera"] ?: "back"
                        val base64 = capturePhoto(facing)
                            ?: error("Failed to capture photo — check CAMERA permission")
                        call.respond(mapOf("format" to "jpeg", "image_base64" to base64))
                    }

                    // ── Files ─────────────────────────────────────────────────
                    get("/files") {
                        val path = call.request.queryParameters["path"]
                        val dir  = if (path != null) File(path)
                        else android.os.Environment.getExternalStorageDirectory()
                        if (!dir.exists() || !dir.isDirectory)
                            error("Invalid path: ${dir.absolutePath}")
                        val files = dir.listFiles()
                            ?.sortedByDescending { it.lastModified() }
                            ?.map { f -> mapOf(
                                "name"         to f.name,
                                "path"         to f.absolutePath,
                                "size_bytes"   to f.length(),
                                "is_directory" to f.isDirectory,
                                "modified"     to Date(f.lastModified()).toString()
                            )} ?: emptyList()
                        call.respond(mapOf("path" to dir.absolutePath, "files" to files))
                    }

                    get("/files/download") {
                        val path = call.request.queryParameters["path"] ?: error("Missing 'path'")
                        val file = File(path)
                        if (!file.exists() || !file.isFile) error("File not found: $path")
                        if (file.length() > 100 * 1024 * 1024) error("File exceeds 100 MB limit")
                        call.response.header(
                            HttpHeaders.ContentDisposition,
                            "attachment; filename=\"${file.name}\""
                        )
                        call.respondFile(file)
                    }

                    put("/files") {
                        val path  = call.request.queryParameters["path"] ?: error("Missing 'path' query param")
                        val file  = File(path)
                        if (!requireApprovalIfOutsideSandbox(call, file, "Overwrite file")) return@put
                        val bytes = call.receive<ByteArray>()
                        file.parentFile?.mkdirs()
                        file.writeBytes(bytes)
                        AuditLog.record(ctx, "files.write", "$path (${bytes.size} bytes)", "ok")
                        call.respond(mapOf("ok" to true, "path" to file.absolutePath, "size_bytes" to bytes.size))
                    }

                    delete("/files") {
                        val path = call.request.queryParameters["path"] ?: error("Missing 'path' query param")
                        val file = File(path)
                        if (!file.exists()) error("File not found: $path")
                        if (!requireApprovalIfOutsideSandbox(call, file, "Delete")) return@delete
                        val deleted = file.deleteRecursively()
                        AuditLog.record(ctx, "files.delete", path, if (deleted) "ok" else "failed")
                        call.respond(mapOf("ok" to deleted, "path" to path))
                    }

                    post("/files/zip") {
                        val body    = call.receive<Map<String, Any>>()
                        @Suppress("UNCHECKED_CAST")
                        val sources = (body["sources"] as? List<String>) ?: error("Missing 'sources' list")
                        val dest    = body["dest"]?.toString() ?: error("Missing 'dest' path")
                        if (!requireApprovalIfOutsideSandbox(call, File(dest), "Create zip file")) return@post
                        zipFiles(sources, dest)
                        AuditLog.record(ctx, "files.zip", "$sources -> $dest", "ok")
                        call.respond(mapOf("ok" to true, "dest" to dest))
                    }

                    // ── Wallpaper ─────────────────────────────────────────────
                    put("/device/wallpaper") {
                        val bytes = call.receive<ByteArray>()
                        val wm    = android.app.WallpaperManager.getInstance(ctx)
                        val bmp   = BitmapFactory.decodeByteArray(bytes, 0, bytes.size)
                        wm.setBitmap(bmp)
                        call.respond(mapOf("ok" to true))
                    }

                    // ── Shell ─────────────────────────────────────────────────
                    post("/shell") {
                        val body      = call.receive<Map<String, Any>>()
                        val command   = body["command"]?.toString() ?: error("Missing 'command'")
                        if (!requireApproval(call, "Run shell command:\n$command")) return@post
                        // Robust boolean parse: handles JSON bool, "true"/"false" string, and absent field.
                        // Defaults to Termux when available so callers don't have to pass termux:true explicitly.
                        val useTermux = when (val t = body["termux"]) {
                            is Boolean -> t
                            is String  -> t.toBooleanStrictOrNull() ?: false
                            else       -> isTermuxAvailable(ctx)   // omitted → auto-detect
                        }
                        val workdir = body["workdir"]?.toString()
                        val result  = runShell(command, useTermux, workdir)
                        AuditLog.record(ctx, "shell.run", command, "ok")
                        call.respond(result)
                    }

                    // ── Agent: tools manifest ─────────────────────────────────
                    // Returns all available tools so the LLM knows what it can call.
                    get("/agent/tools") {
                        // Explicitly typing the list to help compiler inference
                        val tools: List<Map<String, Any>> = listOf(
                            mapOf("name" to "sms.send",             "description" to "Send an SMS to a phone number",                      "params" to listOf("number", "message")),
                            mapOf("name" to "sms.inbox",            "description" to "Read SMS inbox messages",                            "params" to listOf("limit?")),
                            mapOf("name" to "call.make",            "description" to "Make a phone call",                                  "params" to listOf("number")),
                            mapOf("name" to "call.log",             "description" to "Get call history",                                   "params" to listOf("limit?")),
                            mapOf("name" to "contacts.list",        "description" to "List or search contacts by name",                    "params" to listOf("q?", "limit?")),
                            mapOf("name" to "contacts.delete",      "description" to "Delete a contact by id",                            "params" to listOf("id")),
                            mapOf("name" to "battery.get",          "description" to "Get battery level, charging status, temperature",    "params" to listOf<String>()),
                            mapOf("name" to "device.info",          "description" to "Get device model, Android version, IP",              "params" to listOf<String>()),
                            mapOf("name" to "device.volume.get",    "description" to "Get current volume for all audio streams",           "params" to listOf<String>()),
                            mapOf("name" to "device.volume.set",    "description" to "Set volume for a specific audio stream",             "params" to listOf("stream (music|ring|alarm|notification|call)", "level (int)")),
                            mapOf("name" to "device.brightness.set", "description" to "Set screen brightness (disables auto-brightness)", "params" to listOf("level (1-255)")),
                            mapOf("name" to "device.brightness.auto","description" to "Enable or disable automatic brightness",          "params" to listOf("enabled (bool)")),
                            mapOf("name" to "device.ringermode.get","description" to "Get current ringer mode",                           "params" to listOf<String>()),
                            mapOf("name" to "device.ringermode.set","description" to "Set ringer mode",                                    "params" to listOf("mode (silent|vibrate|normal)")),
                            mapOf("name" to "device.tts",           "description" to "Make the phone speak text aloud",                   "params" to listOf("text", "language? (e.g. en-US)")),
                            mapOf("name" to "device.vibrate",       "description" to "Vibrate the device with a pattern",                 "params" to listOf("pattern? (array of ms e.g. [0,300,100,300])")),
                            mapOf("name" to "device.flashlight",    "description" to "Turn flashlight on or off",                         "params" to listOf("enabled (bool)")),
                            mapOf("name" to "device.toast",         "description" to "Show a brief popup message on screen",              "params" to listOf("text")),
                            mapOf("name" to "device.clipboard.get", "description" to "Read current clipboard text",                       "params" to listOf<String>()),
                            mapOf("name" to "device.clipboard.set", "description" to "Write text to clipboard",                           "params" to listOf("text")),
                            mapOf("name" to "device.storage",       "description" to "Get internal and external storage usage",           "params" to listOf<String>()),
                            mapOf("name" to "device.apps",          "description" to "List all installed and launchable apps",            "params" to listOf<String>()),
                            mapOf("name" to "device.audio",         "description" to "Check if audio/music is currently playing",        "params" to listOf<String>()),
                            mapOf("name" to "notification.push",    "description" to "Push a local notification to the device",           "params" to listOf("title", "message", "id?")),
                            mapOf("name" to "launch.app",           "description" to "Launch an installed app by package name",           "params" to listOf("package")),
                            mapOf("name" to "launch.url",           "description" to "Open a URL in the browser",                        "params" to listOf("url")),
                            mapOf("name" to "location.get",         "description" to "Get current GPS coordinates and maps URL",          "params" to listOf<String>()),
                            mapOf("name" to "camera.photo",         "description" to "Take a photo with front or back camera",            "params" to listOf("camera? (back|front)")),
                            mapOf("name" to "files.list",           "description" to "Browse files and folders at a given path",          "params" to listOf("path?")),
                            mapOf("name" to "files.write",          "description" to "Write/overwrite a file from base64 content. Writes inside /sdcard/AgentAPI/ execute immediately; anywhere else requires an on-device approval tap.", "params" to listOf("path", "content_base64")),
                            mapOf("name" to "files.delete",         "description" to "Delete a file or directory recursively. Outside /sdcard/AgentAPI/ requires an on-device approval tap.", "params" to listOf("path")),
                            mapOf("name" to "files.zip",            "description" to "Zip a list of files to a destination path. A dest outside /sdcard/AgentAPI/ requires an on-device approval tap.", "params" to listOf("sources (string array)", "dest (string)")),
                            mapOf("name" to "shell.run",            "description" to "Run a shell command. Requires an on-device approval tap every time. Uses Termux automatically when available (full Linux env, pkg, etc). Pass termux:false to force system shell. Pass workdir to set working directory.", "params" to listOf("command", "termux? (bool, default: auto)", "workdir? (string, default: Termux home)"),),
                            mapOf("name" to "alarm.status",         "description" to "Check if alarm permission is granted and what the next scheduled alarm is. Call this if alarm.set fails.", "params" to listOf<String>()),
                            mapOf("name" to "alarm.set",            "description" to "Set an alarm. Works from background — fires over lock screen regardless of what app is open. Use 24h hour (0-23).", "params" to listOf("hour (0-23)", "minute (0-59)", "label?")),
                            mapOf("name" to "alarm.cancel",         "description" to "Cancel a previously set alarm by hour and minute",  "params" to listOf("hour (0-23)", "minute (0-59)")),
                            mapOf("name" to "media.play",           "description" to "Play an audio file from a local path",              "params" to listOf("path")),
                            mapOf("name" to "media.stop",           "description" to "Stop currently playing media",                      "params" to listOf<String>()),
                            mapOf("name" to "wifi.info",            "description" to "Get WiFi SSID, signal strength and IP address",     "params" to listOf<String>())
                        )
                        call.respond(mapOf("tools" to tools))
                    }

                    // ── Agent: tool executor ──────────────────────────────────
                    // Single endpoint the LLM calls to execute any tool by name.
                    // Body: { "tool": "tool.name", "params": { ...args } }
                    post("/agent/tool") {
                        val body   = call.receive<Map<String, Any>>()
                        val tool   = body["tool"]?.toString() ?: error("Missing 'tool'")
                        // params may arrive as a nested object OR as a JSON string (e.g. axon-agent's
                        // synapse tool, or n8n's $fromAI with type 'json', both serialise the whole
                        // object as a string). Handle both.
                        @Suppress("UNCHECKED_CAST")
                        val params: Map<String, Any> = when (val raw = body["params"]) {
                            is Map<*, *> -> raw as Map<String, Any>
                            is String    -> try { gson.fromJson(raw, Map::class.java) as Map<String, Any> } catch (e: Exception) { emptyMap() }
                            else         -> emptyMap()
                        }

                        // Safe param extractors — throw a clear error instead of
                        // NumberFormatException("For input string: \"null\"") when
                        // a required param is missing or null.
                        fun reqStr(key: String): String =
                            params[key]?.toString()?.takeIf { it != "null" && it.isNotBlank() }
                                ?: error("Missing required param '$key' for tool '$tool'")

                        fun reqInt(key: String): Int =
                            params[key]?.toString()?.takeIf { it != "null" }
                                ?.toDoubleOrNull()?.toInt()
                                ?: error("Param '$key' must be an integer for tool '$tool' (got: ${params[key]})")

                        fun optStr(key: String): String? =
                            params[key]?.toString()?.takeIf { it != "null" && it.isNotBlank() }

                        fun optInt(key: String, default: Int): Int =
                            params[key]?.toString()?.takeIf { it != "null" }
                                ?.toDoubleOrNull()?.toInt() ?: default

                        val result: Any = when (tool) {

                            "sms.send" -> {
                                if (!checkPermission(call, Manifest.permission.SEND_SMS)) return@post
                                val number = reqStr("number")
                                sendSms(number, reqStr("message"))
                                AuditLog.record(ctx, "sms.send", "to=$number", "ok")
                                mapOf("ok" to true)
                            }

                            "sms.inbox" -> {
                                if (!checkPermission(call, Manifest.permission.READ_SMS)) return@post
                                mapOf("messages" to getSmsInbox(
                                    params["limit"]?.toString()?.toIntOrNull() ?: 20, 0
                                ))
                            }

                            "call.make" -> {
                                if (!checkPermission(call, Manifest.permission.CALL_PHONE)) return@post
                                val number = reqStr("number")
                                initiateCall(number)
                                AuditLog.record(ctx, "call.make", "to=$number", "ok")
                                mapOf("ok" to true)
                            }

                            "call.log" -> {
                                if (!checkPermission(call, Manifest.permission.READ_CALL_LOG)) return@post
                                mapOf("calls" to getCallLog(params["limit"]?.toString()?.toIntOrNull() ?: 20))
                            }

                            "contacts.list" -> {
                                if (!checkPermission(call, Manifest.permission.READ_CONTACTS)) return@post
                                mapOf("contacts" to getContacts(
                                    params["q"]?.toString(),
                                    params["limit"]?.toString()?.toIntOrNull() ?: 50
                                ))
                            }

                            "contacts.delete" -> {
                                if (!checkPermission(call, Manifest.permission.WRITE_CONTACTS)) return@post
                                val id = reqStr("id")
                                if (!requireApproval(call, "Delete contact id=$id")) return@post
                                val deleted = deleteContact(id)
                                AuditLog.record(ctx, "contacts.delete", "id=$id", if (deleted) "ok" else "failed")
                                mapOf("ok" to deleted)
                            }

                            "battery.get"           -> getBattery()
                            "device.info"           -> getDevice()
                            "device.volume.get"     -> getVolume(ctx)
                            "device.storage"        -> getStorage()
                            "device.audio"          -> {
                                val am = ctx.getSystemService(Context.AUDIO_SERVICE) as AudioManager
                                mapOf("playing" to am.isMusicActive)
                            }
                            "wifi.info"             -> getWifiInfo(ctx)
                            "location.get"          -> getLocation()

                            "device.ringermode.get" -> {
                                val am = ctx.getSystemService(Context.AUDIO_SERVICE) as AudioManager
                                mapOf("ringer_mode" to when (am.ringerMode) {
                                    AudioManager.RINGER_MODE_SILENT  -> "silent"
                                    AudioManager.RINGER_MODE_VIBRATE -> "vibrate"
                                    else                             -> "normal"
                                })
                            }

                            "device.volume.set" -> {
                                setVolume(ctx,
                                    params["stream"]?.toString() ?: "music",
                                    reqInt("level")
                                )
                                mapOf("ok" to true)
                            }

                            "device.brightness.set" -> {
                                setBrightness(ctx, reqInt("level"))
                                mapOf("ok" to true)
                            }

                            "device.brightness.auto" -> {
                                setAutoBrightness(ctx, reqStr("enabled").toBoolean())
                                mapOf("ok" to true)
                            }

                            "device.ringermode.set" -> {
                                setRingerMode(ctx, reqStr("mode"))
                                mapOf("ok" to true)
                            }

                            "device.tts" -> {
                                speakText(reqStr("text"), optStr("language"))
                                mapOf("ok" to true)
                            }

                            "device.vibrate" -> {
                                @Suppress("UNCHECKED_CAST")
                                val pattern = (params["pattern"] as? List<Double>)
                                    ?.map { it.toLong() }?.toLongArray()
                                    ?: longArrayOf(0, 300)
                                vibrateDevice(ctx, pattern)
                                mapOf("ok" to true)
                            }

                            "device.flashlight" -> {
                                setFlashlight(ctx, reqStr("enabled").toBoolean())
                                mapOf("ok" to true)
                            }

                            "device.toast" -> {
                                val text = reqStr("text")
                                Handler(ctx.mainLooper).post {
                                    android.widget.Toast.makeText(ctx, text, android.widget.Toast.LENGTH_LONG).show()
                                }
                                mapOf("ok" to true)
                            }

                            "device.clipboard.get" -> {
                                val cm = ctx.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                                mapOf("text" to cm.primaryClip?.getItemAt(0)?.text?.toString())
                            }

                            "device.clipboard.set" -> {
                                val cm = ctx.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                                cm.setPrimaryClip(ClipData.newPlainText("agent", reqStr("text")))
                                mapOf("ok" to true)
                            }

                            "device.apps" -> {
                                val pm   = ctx.packageManager
                                val apps = pm.getInstalledApplications(PackageManager.GET_META_DATA)
                                    .filter { pm.getLaunchIntentForPackage(it.packageName) != null }
                                    .map { mapOf("name" to pm.getApplicationLabel(it).toString(), "package" to it.packageName) }
                                    .sortedBy { it["name"] as String }
                                mapOf("count" to apps.size, "apps" to apps)
                            }

                            "notification.push" -> {
                                pushNotification(ctx,
                                    reqStr("title"),
                                    reqStr("message"),
                                    (params["id"] as? Double)?.toInt() ?: (System.currentTimeMillis() % Int.MAX_VALUE).toInt()
                                )
                                mapOf("ok" to true)
                            }

                            "launch.app" -> {
                                if (!android.provider.Settings.canDrawOverlays(ctx)) {
                                    error(
                                        "Missing 'Draw over other apps' permission (SYSTEM_ALERT_WINDOW). " +
                                        "Android 10+ blocks background activity launches without it. " +
                                        "Go to: Settings → Apps → AgentAPI → Display over other apps → Enable"
                                    )
                                }
                                val pkg = reqStr("package")
                                // Strategy 1: standard launch intent (works for most apps)
                                var intent = ctx.packageManager.getLaunchIntentForPackage(pkg)
                                    ?.apply { addFlags(Intent.FLAG_ACTIVITY_NEW_TASK) }
                                // Strategy 2: query activities with MAIN/LAUNCHER for this package
                                // (catches apps like Gmail where getLaunchIntentForPackage returns null
                                //  even when installed, due to split APKs or custom entry points)
                                if (intent == null) {
                                    val mainIntent = Intent(Intent.ACTION_MAIN).apply {
                                        addCategory(Intent.CATEGORY_LAUNCHER)
                                        setPackage(pkg)
                                        addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                                    }
                                    val resolved = ctx.packageManager
                                        .queryIntentActivities(mainIntent, 0)
                                    if (resolved.isNotEmpty()) {
                                        intent = mainIntent.apply {
                                            component = android.content.ComponentName(
                                                pkg,
                                                resolved[0].activityInfo.name
                                            )
                                        }
                                    }
                                }
                                intent ?: error("App not found or not launchable: $pkg — is it installed?")
                                ctx.startActivity(intent)
                                mapOf("ok" to true, "package" to pkg)
                            }

                            "launch.url" -> {
                                if (!android.provider.Settings.canDrawOverlays(ctx)) {
                                    error(
                                        "Missing 'Draw over other apps' permission (SYSTEM_ALERT_WINDOW). " +
                                        "Android 10+ blocks background activity launches without it. " +
                                        "Go to: Settings → Apps → AgentAPI → Display over other apps → Enable"
                                    )
                                }
                                val url = reqStr("url")
                                ctx.startActivity(
                                    Intent(Intent.ACTION_VIEW, Uri.parse(url))
                                        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                                )
                                mapOf("ok" to true, "url" to url)
                            }

                            "camera.photo" -> {
                                if (!checkPermission(call, Manifest.permission.CAMERA)) return@post
                                val base64 = capturePhoto(params["camera"]?.toString() ?: "back")
                                    ?: error("Failed to capture photo")
                                mapOf("format" to "jpeg", "image_base64" to base64)
                            }

                            "files.list" -> {
                                val dir = File(params["path"]?.toString()
                                    ?: android.os.Environment.getExternalStorageDirectory().absolutePath)
                                mapOf("files" to (dir.listFiles()
                                    ?.sortedByDescending { it.lastModified() }
                                    ?.map { mapOf(
                                        "name"         to it.name,
                                        "path"         to it.absolutePath,
                                        "size_bytes"   to it.length(),
                                        "is_directory" to it.isDirectory,
                                        "modified"     to Date(it.lastModified()).toString()
                                    )} ?: emptyList<Any>()))
                            }

                            "files.write" -> {
                                val path = reqStr("path")
                                val file = File(path)
                                if (!requireApprovalIfOutsideSandbox(call, file, "Overwrite file")) return@post
                                val bytes = Base64.decode(reqStr("content_base64"), Base64.DEFAULT)
                                file.parentFile?.mkdirs()
                                file.writeBytes(bytes)
                                AuditLog.record(ctx, "files.write", "$path (${bytes.size} bytes)", "ok")
                                mapOf("ok" to true, "path" to file.absolutePath, "size_bytes" to bytes.size)
                            }

                            "files.delete" -> {
                                val path = reqStr("path")
                                val f = File(path)
                                if (!f.exists()) error("File not found: $path")
                                if (!requireApprovalIfOutsideSandbox(call, f, "Delete")) return@post
                                val deleted = f.deleteRecursively()
                                AuditLog.record(ctx, "files.delete", path, if (deleted) "ok" else "failed")
                                mapOf("ok" to deleted)
                            }

                            "files.zip" -> {
                                @Suppress("UNCHECKED_CAST")
                                val sources = params["sources"] as? List<String> ?: error("Missing 'sources'")
                                val dest    = params["dest"]?.toString() ?: error("Missing 'dest'")
                                if (!requireApprovalIfOutsideSandbox(call, File(dest), "Create zip file")) return@post
                                zipFiles(sources, dest)
                                AuditLog.record(ctx, "files.zip", "$sources -> $dest", "ok")
                                mapOf("ok" to true)
                            }

                            "shell.run" -> {
                                val command = reqStr("command")
                                if (!requireApproval(call, "Run shell command:\n$command")) return@post
                                val useTermux = when (val t = params["termux"]) {
                                    is Boolean -> t
                                    is String  -> t.toBooleanStrictOrNull() ?: false
                                    else       -> isTermuxAvailable(ctx)   // omitted → auto-detect
                                }
                                val res = runShell(command, useTermux, params["workdir"]?.toString())
                                AuditLog.record(ctx, "shell.run", command, "ok")
                                res
                            }
                            "alarm.status" -> {
                                val am = ctx.getSystemService(Context.ALARM_SERVICE) as AlarmManager
                                val canSchedule = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S)
                                    am.canScheduleExactAlarms() else true
                                val nextAlarm = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.LOLLIPOP) {
                                    am.nextAlarmClock?.let {
                                        mapOf("trigger_time" to java.util.Date(it.triggerTime).toString(),
                                            "trigger_ms" to it.triggerTime)
                                    }
                                } else null
                                mapOf(
                                    "can_schedule_exact_alarms" to canSchedule,
                                    "next_alarm" to nextAlarm,
                                    "setup_required" to !canSchedule,
                                    "setup_instructions" to if (!canSchedule)
                                        "On the phone: Settings → Apps → AgentAPI → Alarms & reminders → Allow"
                                    else "OK — alarm permission granted"
                                )
                            }

                            "alarm.set" -> {
                                setAlarm(ctx,
                                    reqInt("hour"),
                                    reqInt("minute"),
                                    params["label"]?.toString() ?: "Agent Alarm"
                                )
                                mapOf("ok" to true)
                            }

                            "alarm.cancel" -> {
                                cancelAlarm(ctx,
                                    reqInt("hour"),
                                    reqInt("minute")
                                )
                                mapOf("ok" to true)
                            }

                            "media.play" -> {
                                playMedia(reqStr("path"))
                                mapOf("ok" to true)
                            }

                            "media.stop" -> {
                                stopMedia()
                                mapOf("ok" to true)
                            }

                            else -> {
                                // Return the full tool list in the error so the LLM can self-correct
                                // without needing a separate GET /agent/tools call.
                                val knownTools = listOf(
                                    "sms.send", "sms.inbox", "call.make", "call.log",
                                    "contacts.list", "contacts.delete", "battery.get",
                                    "device.info", "device.volume.get", "device.volume.set",
                                    "device.brightness.set", "device.ringermode.get", "device.ringermode.set",
                                    "device.tts", "device.vibrate", "device.flashlight", "device.toast",
                                    "device.clipboard.get", "device.clipboard.set", "device.storage",
                                    "device.apps", "device.audio", "notification.push",
                                    "launch.app", "launch.url", "location.get", "camera.photo",
                                    "files.list", "files.write", "files.delete", "files.zip", "shell.run",
                                    "alarm.status", "alarm.set", "alarm.cancel",
                                    "media.play", "media.stop", "wifi.info"
                                )
                                call.respond(HttpStatusCode.BadRequest, mapOf(
                                    "error"        to "Unknown tool: '$tool'",
                                    "known_tools"  to knownTools,
                                    "hint"         to "Use one of the known_tools values exactly as listed. Call GET /agent/tools for full parameter details."
                                ))
                                return@post
                            }
                        }

                        call.respond(result)
                    }

                } // end authenticate
            } // end routing
        }

        engine!!.start(wait = false)
        Log.i(TAG, "Ktor server started on port $port")
    }

    fun stop() {
        tts?.stop()
        tts?.shutdown()
        tts = null
        mediaPlayer?.release()
        mediaPlayer = null
        engine?.stop(gracePeriodMillis = 500, timeoutMillis = 2000)
        engine = null
        Log.i(TAG, "Ktor server stopped")
    }

    fun isRunning() = engine != null

    // ── Permission helper ─────────────────────────────────────────────────────

    private suspend fun checkPermission(call: ApplicationCall, permission: String): Boolean {
        if (ContextCompat.checkSelfPermission(context, permission)
            != PackageManager.PERMISSION_GRANTED) {
            call.respond(HttpStatusCode.Forbidden,
                mapOf("error" to "Permission not granted: $permission"))
            return false
        }
        return true
    }

    // ── SMS ───────────────────────────────────────────────────────────────────

    @Suppress("DEPRECATION")
    private fun getSmsManager(): SmsManager =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S)
            context.getSystemService(SmsManager::class.java)
        else SmsManager.getDefault()

    private fun sendSms(number: String, message: String) {
        val sm    = getSmsManager()
        val parts = sm.divideMessage(message)
        if (parts.size > 1) sm.sendMultipartTextMessage(number, null, parts, null, null)
        else sm.sendTextMessage(number, null, message, null, null)
    }

    @SuppressLint("MissingPermission")
    private fun getSmsInbox(limit: Int, offset: Int): List<Map<String, Any?>> {
        val results = mutableListOf<Map<String, Any?>>()
        context.contentResolver.query(
            Telephony.Sms.CONTENT_URI, null, null, null,
            "${Telephony.Sms.DATE} DESC"
        )?.use { c ->
            var skipped = 0
            while (c.moveToNext() && results.size < limit) {
                if (skipped++ < offset) continue
                results += mapOf(
                    "id"      to c.getString(c.getColumnIndexOrThrow(Telephony.Sms._ID)),
                    "address" to c.getString(c.getColumnIndexOrThrow(Telephony.Sms.ADDRESS)),
                    "body"    to c.getString(c.getColumnIndexOrThrow(Telephony.Sms.BODY)),
                    "date"    to c.getLong(c.getColumnIndexOrThrow(Telephony.Sms.DATE)),
                    "type"    to when (c.getInt(c.getColumnIndexOrThrow(Telephony.Sms.TYPE))) {
                        Telephony.Sms.MESSAGE_TYPE_INBOX -> "inbox"
                        Telephony.Sms.MESSAGE_TYPE_SENT  -> "sent"
                        else -> "other"
                    },
                    "read"    to (c.getInt(c.getColumnIndexOrThrow(Telephony.Sms.READ)) == 1)
                )
            }
        }
        return results
    }

    // ── Calls ─────────────────────────────────────────────────────────────────
    @SuppressLint("MissingPermission")
    private fun initiateCall(number: String) {
        val clean = number.replace(Regex("[^+0-9]"), "")
        val uri   = Uri.fromParts("tel", clean, null)

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val telecom = context.getSystemService(TelecomManager::class.java)
            if (ContextCompat.checkSelfPermission(context, Manifest.permission.CALL_PHONE)
                == PackageManager.PERMISSION_GRANTED) {
                telecom.placeCall(uri, android.os.Bundle())
                return
            }
        }
        val intent = Intent(Intent.ACTION_CALL, uri).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
        }
        context.startActivity(intent)
    }

    @SuppressLint("MissingPermission")
    private fun getCallLog(limit: Int): List<Map<String, Any?>> {
        val results = mutableListOf<Map<String, Any?>>()
        context.contentResolver.query(
            CallLog.Calls.CONTENT_URI, null, null, null,
            "${CallLog.Calls.DATE} DESC"
        )?.use { c ->
            while (c.moveToNext() && results.size < limit) {
                results += mapOf(
                    "number"   to c.getString(c.getColumnIndexOrThrow(CallLog.Calls.NUMBER)),
                    "name"     to c.getString(c.getColumnIndexOrThrow(CallLog.Calls.CACHED_NAME)),
                    "type"     to when (c.getInt(c.getColumnIndexOrThrow(CallLog.Calls.TYPE))) {
                        CallLog.Calls.INCOMING_TYPE -> "incoming"
                        CallLog.Calls.OUTGOING_TYPE -> "outgoing"
                        CallLog.Calls.MISSED_TYPE   -> "missed"
                        CallLog.Calls.REJECTED_TYPE -> "rejected"
                        else -> "unknown"
                    },
                    "date"     to c.getLong(c.getColumnIndexOrThrow(CallLog.Calls.DATE)),
                    "duration" to c.getLong(c.getColumnIndexOrThrow(CallLog.Calls.DURATION))
                )
            }
        }
        return results
    }
    @SuppressLint("MissingPermission")
    private fun clearCallLog(): Int =
        context.contentResolver.delete(CallLog.Calls.CONTENT_URI, null, null)

    // ── Contacts ──────────────────────────────────────────────────────────────
    @SuppressLint("MissingPermission")
    private fun getContacts(search: String?, limit: Int): List<Map<String, Any?>> {
        val results   = mutableListOf<Map<String, Any?>>()
        val selection = if (!search.isNullOrBlank())
            "${ContactsContract.Contacts.DISPLAY_NAME} LIKE ?" else null
        val selArgs   = if (!search.isNullOrBlank()) arrayOf("%$search%") else null

        context.contentResolver.query(
            ContactsContract.Contacts.CONTENT_URI,
            arrayOf(
                ContactsContract.Contacts._ID,
                ContactsContract.Contacts.DISPLAY_NAME,
                ContactsContract.Contacts.HAS_PHONE_NUMBER
            ),
            selection, selArgs,
            "${ContactsContract.Contacts.DISPLAY_NAME} ASC"
        )?.use { c ->
            while (c.moveToNext() && results.size < limit) {
                val id     = c.getString(0)
                val name   = c.getString(1)
                val phones = mutableListOf<String>()
                if (c.getInt(2) > 0) {
                    context.contentResolver.query(
                        ContactsContract.CommonDataKinds.Phone.CONTENT_URI,
                        arrayOf(ContactsContract.CommonDataKinds.Phone.NUMBER),
                        "${ContactsContract.CommonDataKinds.Phone.CONTACT_ID} = ?",
                        arrayOf(id), null
                    )?.use { pc -> while (pc.moveToNext()) phones += pc.getString(0) }
                }
                results += mapOf("id" to id, "name" to name, "phones" to phones)
            }
        }
        return results
    }
    @SuppressLint("MissingPermission")
    private fun deleteContact(id: String): Boolean {
        val uri = Uri.withAppendedPath(ContactsContract.Contacts.CONTENT_URI, id)
        return context.contentResolver.delete(uri, null, null) > 0
    }

    // ── Battery ───────────────────────────────────────────────────────────────

    private fun getBattery(): Map<String, Any?> {
        val intent  = context.registerReceiver(null,
            IntentFilter(Intent.ACTION_BATTERY_CHANGED)) ?: return mapOf("error" to "unavailable")
        val level   = intent.getIntExtra(BatteryManager.EXTRA_LEVEL, -1)
        val scale   = intent.getIntExtra(BatteryManager.EXTRA_SCALE, -1)
        val status  = intent.getIntExtra(BatteryManager.EXTRA_STATUS, -1)
        val plugged = intent.getIntExtra(BatteryManager.EXTRA_PLUGGED, -1)
        return mapOf(
            "percent"     to if (scale > 0) (level * 100 / scale) else -1,
            "charging"    to (status == BatteryManager.BATTERY_STATUS_CHARGING
                    || status == BatteryManager.BATTERY_STATUS_FULL),
            "plugged"     to when (plugged) {
                BatteryManager.BATTERY_PLUGGED_AC       -> "AC"
                BatteryManager.BATTERY_PLUGGED_USB      -> "USB"
                BatteryManager.BATTERY_PLUGGED_WIRELESS -> "wireless"
                else -> "unplugged"
            },
            "temperature" to intent.getIntExtra(BatteryManager.EXTRA_TEMPERATURE, 0) / 10.0,
            "voltage_mv"  to intent.getIntExtra(BatteryManager.EXTRA_VOLTAGE, -1)
        )
    }

    // ── Device ────────────────────────────────────────────────────────────────

    private fun getDevice(): Map<String, Any?> = mapOf(
        "manufacturer" to Build.MANUFACTURER,
        "brand"        to Build.BRAND,
        "model"        to Build.MODEL,
        "device"       to Build.DEVICE,
        "android"      to Build.VERSION.RELEASE,
        "sdk"          to Build.VERSION.SDK_INT,
        "termux"       to isTermuxAvailable(context),
        "termux_path"  to (findTermuxBash() ?: "not found"),
        "local_ip"     to getLocalIp()
    )

    // ── Volume ────────────────────────────────────────────────────────────────

    private fun getVolume(ctx: Context): Map<String, Any> {
        val am = ctx.getSystemService(Context.AUDIO_SERVICE) as AudioManager
        return mapOf(
            "music"        to mapOf("current" to am.getStreamVolume(AudioManager.STREAM_MUSIC),
                "max"     to am.getStreamMaxVolume(AudioManager.STREAM_MUSIC)),
            "ring"         to mapOf("current" to am.getStreamVolume(AudioManager.STREAM_RING),
                "max"     to am.getStreamMaxVolume(AudioManager.STREAM_RING)),
            "alarm"        to mapOf("current" to am.getStreamVolume(AudioManager.STREAM_ALARM),
                "max"     to am.getStreamMaxVolume(AudioManager.STREAM_ALARM)),
            "notification" to mapOf("current" to am.getStreamVolume(AudioManager.STREAM_NOTIFICATION),
                "max"     to am.getStreamMaxVolume(AudioManager.STREAM_NOTIFICATION)),
            "call"         to mapOf("current" to am.getStreamVolume(AudioManager.STREAM_VOICE_CALL),
                "max"     to am.getStreamMaxVolume(AudioManager.STREAM_VOICE_CALL))
        )
    }

    private fun setVolume(ctx: Context, stream: String, level: Int) {
        val am = ctx.getSystemService(Context.AUDIO_SERVICE) as AudioManager
        val streamType = when (stream) {
            "ring"         -> AudioManager.STREAM_RING
            "alarm"        -> AudioManager.STREAM_ALARM
            "notification" -> AudioManager.STREAM_NOTIFICATION
            "call"         -> AudioManager.STREAM_VOICE_CALL
            else           -> AudioManager.STREAM_MUSIC
        }
        am.setStreamVolume(streamType, level, 0)
    }

    // ── Brightness ────────────────────────────────────────────────────────────

    private fun setBrightness(ctx: Context, level: Int) {
        // WRITE_SETTINGS is a special permission — cannot be granted via requestPermissions().
        // User must grant it manually: Settings → Apps → AgentAPI → Modify System Settings → Enable
        if (!android.provider.Settings.System.canWrite(ctx)) {
            error(
                "Missing 'Modify System Settings' permission (WRITE_SETTINGS). " +
                "Go to: Settings → Apps → AgentAPI → Modify System Settings → Enable"
            )
        }
        // Also disable auto-brightness first, otherwise the system overrides our value
        android.provider.Settings.System.putInt(
            ctx.contentResolver,
            android.provider.Settings.System.SCREEN_BRIGHTNESS_MODE,
            android.provider.Settings.System.SCREEN_BRIGHTNESS_MODE_MANUAL
        )
        android.provider.Settings.System.putInt(
            ctx.contentResolver,
            android.provider.Settings.System.SCREEN_BRIGHTNESS,
            level.coerceIn(1, 255)  // 0 is floored to min-visible by Android; use 1 as practical minimum
        )
    }

    private fun setAutoBrightness(ctx: Context, enabled: Boolean) {
        if (!android.provider.Settings.System.canWrite(ctx)) {
            error(
                "Missing 'Modify System Settings' permission (WRITE_SETTINGS). " +
                "Go to: Settings → Apps → AgentAPI → Modify System Settings → Enable"
            )
        }
        android.provider.Settings.System.putInt(
            ctx.contentResolver,
            android.provider.Settings.System.SCREEN_BRIGHTNESS_MODE,
            if (enabled) android.provider.Settings.System.SCREEN_BRIGHTNESS_MODE_AUTOMATIC
            else         android.provider.Settings.System.SCREEN_BRIGHTNESS_MODE_MANUAL
        )
    }

    // ── Ringer mode ──────────────────────────────────────────────────────────────

    private fun setRingerMode(ctx: Context, mode: String) {
        val am = ctx.getSystemService(Context.AUDIO_SERVICE) as AudioManager
        am.ringerMode = when (mode) {
            "silent"  -> AudioManager.RINGER_MODE_SILENT
            "vibrate" -> AudioManager.RINGER_MODE_VIBRATE
            else      -> AudioManager.RINGER_MODE_NORMAL
        }
    }

    // ── Storage ───────────────────────────────────────────────────────────────

    private fun getStorage(): Map<String, Any?> {
        val internal = StatFs(android.os.Environment.getDataDirectory().path)
        val iTotal   = internal.blockCountLong * internal.blockSizeLong
        val iFree    = internal.availableBlocksLong * internal.blockSizeLong

        val extDir   = android.os.Environment.getExternalStorageDirectory()
        val external = if (extDir.exists()) {
            val e = StatFs(extDir.path)
            mapOf(
                "total_bytes" to e.blockCountLong * e.blockSizeLong,
                "free_bytes"  to e.availableBlocksLong * e.blockSizeLong
            )
        } else null

        return mapOf(
            "internal" to mapOf("total_bytes" to iTotal, "free_bytes" to iFree),
            "external" to external
        )
    }

    // ── WiFi info ─────────────────────────────────────────────────────────────
    @SuppressLint("MissingPermission")
    @Suppress("DEPRECATION")
    private fun getWifiInfo(ctx: Context): Map<String, Any?> {
        val wm   = ctx.applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
        val info = wm.connectionInfo
        val ssid = info.ssid?.removeSurrounding("\"") ?: "unknown"
        val rssi = info.rssi
        val ip   = android.text.format.Formatter.formatIpAddress(info.ipAddress)
        return mapOf(
            "connected"   to wm.isWifiEnabled,
            "ssid"        to ssid,
            "signal_dbm"  to rssi,
            "ip"          to ip,
            "mac"         to info.macAddress
        )
    }

    // ── Notifications ─────────────────────────────────────────────────────────

    private fun pushNotification(ctx: Context, title: String, message: String, id: Int) {
        val nm = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        val notification = NotificationCompat.Builder(ctx, NOTIF_CHANNEL_ID)
            .setSmallIcon(android.R.drawable.ic_dialog_info)
            .setContentTitle(title)
            .setContentText(message)
            .setStyle(NotificationCompat.BigTextStyle().bigText(message))
            .setPriority(NotificationCompat.PRIORITY_DEFAULT)
            .setAutoCancel(true)
            .build()
        nm.notify(id, notification)
    }

    // ── TTS ───────────────────────────────────────────────────────────────────

    private fun speakText(text: String, language: String?) {
        if (language != null) {
            tts?.language = Locale.forLanguageTag(language)
        }
        tts?.speak(text, TextToSpeech.QUEUE_FLUSH, null, "agent_tts_${System.currentTimeMillis()}")
    }

    // ── Vibrate ───────────────────────────────────────────────────────────────

    @Suppress("DEPRECATION")
    private fun vibrateDevice(ctx: Context, pattern: LongArray) {
        val vibrator = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            val vm = ctx.getSystemService(android.os.VibratorManager::class.java)
            vm.defaultVibrator
        } else {
            ctx.getSystemService(Context.VIBRATOR_SERVICE) as android.os.Vibrator
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            vibrator.vibrate(android.os.VibrationEffect.createWaveform(pattern, -1))
        } else {
            @Suppress("DEPRECATION")
            vibrator.vibrate(pattern, -1)
        }
    }

    // ── Flashlight ────────────────────────────────────────────────────────────

    private fun setFlashlight(ctx: Context, enable: Boolean) {
        if (ContextCompat.checkSelfPermission(ctx, Manifest.permission.CAMERA)
            != PackageManager.PERMISSION_GRANTED) {
            error("CAMERA permission not granted — required for flashlight")
        }
        try {
            val cm       = ctx.getSystemService(Context.CAMERA_SERVICE) as CameraManager
            val cameraId = cm.cameraIdList.firstOrNull {
                cm.getCameraCharacteristics(it)
                    .get(CameraCharacteristics.FLASH_INFO_AVAILABLE) == true
            } ?: error("No flash available on this device")
            cm.setTorchMode(cameraId, enable)
        } catch (e: SecurityException) {
            error("Flashlight permission denied: ${e.message}")
        }
    }

    // ── Alarm ─────────────────────────────────────────────────────────────────

    /**
     * Returns true if the app can schedule exact alarms on this device.
     *
     * On Android 12+ (API 31+), AlarmManager.setAlarmClock() silently does nothing
     * if this permission is not granted — no exception is thrown, no alarm is set.
     * On older Android the permission is always implicitly granted.
     *
     * The user grants this at:
     *   Settings → Apps → [AgentAPI] → Alarms & reminders  (Android 12)
     *   Settings → Apps → Special app access → Alarms & reminders (Android 13+)
     */
    private fun canScheduleExactAlarms(ctx: Context): Boolean {
        val am = ctx.getSystemService(Context.ALARM_SERVICE) as AlarmManager
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            am.canScheduleExactAlarms()
        } else {
            true  // always granted below API 31
        }
    }

    /**
     * Sets an alarm using AlarmManager.setAlarmClock().
     *
     * WHY NOT ACTION_SET_ALARM:
     * ACTION_SET_ALARM launches an Activity. Android 10+ background activity
     * start restrictions silently block this whenever another app is in the
     * foreground — the API call returns ok but no alarm is ever set.
     *
     * WHY AlarmManager.setAlarmClock():
     * - Works 100% from a background Service, no foreground restrictions
     * - Fires AlarmRingerReceiver (BroadcastReceiver) which posts a notification
     *   with a fullScreenIntent pointing to AlarmDismissActivity
     * - fullScreenIntent has its own exemption from background start restrictions
     *   → the alarm screen appears over the lock screen and over any foreground app
     * - Shows ⏰ in the status bar just like Clock app alarms
     * - Exact delivery guaranteed even in Doze mode (highest-priority alarm type)
     *
     * Throws IllegalStateException with clear instructions if the
     * SCHEDULE_EXACT_ALARM permission has not been granted by the user.
     */
    private fun setAlarm(ctx: Context, hour: Int, minute: Int, label: String) {
        // ── Permission guard ──────────────────────────────────────────────────
        // On Android 12+, setAlarmClock() silently fails without this permission.
        // We throw here so axon-agent gets a clear error instead of a silent non-event.
        if (!canScheduleExactAlarms(ctx)) {
            // Post a persistent notification so the user knows what to do
            pushAlarmPermissionNotification(ctx)
            error(
                "ALARM_PERMISSION_MISSING: The 'Alarms & reminders' permission has not been granted. " +
                "On the phone, go to: Settings → Apps → AgentAPI → Alarms & reminders → Allow. " +
                "Or open AgentAPI and tap the banner that appears. Then retry."
            )
        }

        val am = ctx.getSystemService(Context.ALARM_SERVICE) as AlarmManager

        val calendar = Calendar.getInstance().apply {
            set(Calendar.HOUR_OF_DAY, hour)
            set(Calendar.MINUTE, minute)
            set(Calendar.SECOND, 0)
            set(Calendar.MILLISECOND, 0)
            // If this time already passed today, schedule for tomorrow
            if (timeInMillis <= System.currentTimeMillis()) add(Calendar.DAY_OF_YEAR, 1)
        }

        // Fires AlarmRingerReceiver at alarm time → launches AlarmDismissActivity
        val receiverIntent = Intent(ctx, AlarmRingerReceiver::class.java).apply {
            putExtra("label", label)
        }
        val alarmPi = PendingIntent.getBroadcast(
            ctx,
            hour * 100 + minute,   // unique slot per time, allows multiple alarms
            receiverIntent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )

        // Tapping the ⏰ status bar icon opens the permission settings directly
        val showPi = PendingIntent.getActivity(
            ctx,
            0,
            Intent(ctx, DeviceApiActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE
        )

        // setAlarmClock: highest-priority alarm type, Doze-exempt, shows clock icon
        am.setAlarmClock(AlarmManager.AlarmClockInfo(calendar.timeInMillis, showPi), alarmPi)

        Log.i(TAG, "Alarm set — ${hour.toString().padStart(2,'0')}:${minute.toString().padStart(2,'0')} " +
            "label='$label' fires=${java.util.Date(calendar.timeInMillis)}")
    }

    /**
     * Posts a persistent notification telling the user to grant Alarms & reminders.
     * Tapping it opens the exact alarm permission settings screen directly.
     */
    private fun pushAlarmPermissionNotification(ctx: Context) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) return

        val nm = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as android.app.NotificationManager

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val ch = android.app.NotificationChannel(
                "alarm_perm_channel", "Alarm Setup",
                android.app.NotificationManager.IMPORTANCE_HIGH
            )
            nm.createNotificationChannel(ch)
        }

        val settingsIntent = Intent(android.provider.Settings.ACTION_REQUEST_SCHEDULE_EXACT_ALARM).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        val pi = PendingIntent.getActivity(
            ctx, 0, settingsIntent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )

        val notif = NotificationCompat.Builder(ctx, "alarm_perm_channel")
            .setSmallIcon(android.R.drawable.ic_lock_idle_alarm)
            .setContentTitle("⚠️ Alarm permission required")
            .setContentText("Tap to grant 'Alarms & reminders' so AgentAPI can set alarms.")
            .setStyle(NotificationCompat.BigTextStyle()
                .bigText("AgentAPI needs the 'Alarms & reminders' permission to set alarms. " +
                    "Tap this notification to grant it, then retry."))
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setContentIntent(pi)
            .setAutoCancel(true)
            .build()

        nm.notify(8999, notif)
    }

    /**
     * Cancels an alarm set by setAlarm(). Reconstructs the same PendingIntent
     * (same request code = hour*100+minute) and calls AlarmManager.cancel().
     * Also cancels the dismiss notification if the alarm is currently ringing.
     */
    private fun cancelAlarm(ctx: Context, hour: Int, minute: Int) {
        val am = ctx.getSystemService(Context.ALARM_SERVICE) as AlarmManager

        val pi = PendingIntent.getBroadcast(
            ctx,
            hour * 100 + minute,
            Intent(ctx, AlarmRingerReceiver::class.java),
            PendingIntent.FLAG_NO_CREATE or PendingIntent.FLAG_IMMUTABLE
        )
        if (pi != null) {
            am.cancel(pi)
            pi.cancel()
            Log.i(TAG, "Alarm cancelled — ${hour.toString().padStart(2,'0')}:${minute.toString().padStart(2,'0')}")
        } else {
            Log.w(TAG, "cancelAlarm: no alarm found for $hour:$minute")
        }

        // Dismiss notification if alarm is actively ringing
        val nm = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as android.app.NotificationManager
        nm.cancel(AlarmRingerReceiver.NOTIF_ID)
    }

    // ── Media playback ────────────────────────────────────────────────────────

    private var mediaPlayer: MediaPlayer? = null

    private fun playMedia(path: String) {
        mediaPlayer?.release()
        mediaPlayer = MediaPlayer().apply {
            setDataSource(path)
            prepare()
            start()
            setOnCompletionListener { release(); mediaPlayer = null }
        }
    }

    private fun stopMedia() {
        mediaPlayer?.stop()
        mediaPlayer?.release()
        mediaPlayer = null
    }

    // ── Zip files ─────────────────────────────────────────────────────────────

    private fun zipFiles(sources: List<String>, dest: String) {
        val destFile = File(dest)
        destFile.parentFile?.mkdirs()
        ZipOutputStream(destFile.outputStream().buffered()).use { zos ->
            sources.forEach { srcPath ->
                val src = File(srcPath)
                if (src.exists()) {
                    zos.putNextEntry(ZipEntry(src.name))
                    src.inputStream().use { it.copyTo(zos) }
                    zos.closeEntry()
                }
            }
        }
    }

    // ── Location ──────────────────────────────────────────────────────────────

    private fun getLocation(): Map<String, Any?> {
        val cached = ApiService.getCachedLocation()
        if (cached != null) {
            val ageSec = (System.currentTimeMillis() - cached.time) / 1000
            return mapOf(
                "available"       to true,
                "source"          to "gps_cache",
                "latitude"        to cached.latitude,
                "longitude"       to cached.longitude,
                "accuracy_meters" to cached.accuracy,
                "altitude"        to cached.altitude,
                "speed_mps"       to if (cached.hasSpeed()) cached.speed else null,
                "bearing"         to if (cached.hasBearing()) cached.bearing else null,
                "provider"        to cached.provider,
                "age_seconds"     to ageSec,
                "time"            to Date(cached.time).toString(),
                "maps_url"        to "https://www.google.com/maps?q=${cached.latitude},${cached.longitude}"
            )
        }
        return mapOf(
            "available" to false,
            "message"   to "No cached location yet. Grant ACCESS_FINE_LOCATION and wait ~60s."
        )
    }

    // ── Camera ────────────────────────────────────────────────────────────────

    private fun capturePhoto(facingHint: String): String? {
        val manager  = context.getSystemService(Context.CAMERA_SERVICE) as CameraManager
        val cameraId = selectCamera(manager, facingHint) ?: return null
        val chars    = manager.getCameraCharacteristics(cameraId)
        val map      = chars.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP) ?: return null
        val sizes    = map.getOutputSizes(ImageFormat.JPEG)
        val size     = sizes[sizes.size / 2]

        val thread     = HandlerThread("CameraThread").also { it.start() }
        val handler    = Handler(thread.looper)
        val reader     = ImageReader.newInstance(size.width, size.height, ImageFormat.JPEG, 2)
        val imageLatch = CountDownLatch(1)
        val result     = AtomicReference<String?>(null)

        reader.setOnImageAvailableListener({ r ->
            r.acquireLatestImage()?.use { img ->
                val buf   = img.planes[0].buffer
                val bytes = ByteArray(buf.remaining()).also { buf.get(it) }
                result.set(Base64.encodeToString(bytes, Base64.NO_WRAP))
            }
            imageLatch.countDown()
        }, handler)

        val openLatch = CountDownLatch(1)
        val cameraRef = AtomicReference<CameraDevice?>(null)

        manager.openCamera(cameraId, object : CameraDevice.StateCallback() {
            override fun onOpened(camera: CameraDevice) { cameraRef.set(camera); openLatch.countDown() }
            override fun onDisconnected(camera: CameraDevice) { camera.close(); openLatch.countDown() }
            override fun onError(camera: CameraDevice, error: Int) { camera.close(); openLatch.countDown() }
        }, handler)

        if (!openLatch.await(5, TimeUnit.SECONDS) || cameraRef.get() == null) {
            thread.quitSafely(); reader.close(); return null
        }

        val camera  = cameraRef.get()!!
        val request = camera.createCaptureRequest(CameraDevice.TEMPLATE_STILL_CAPTURE)
            .apply { addTarget(reader.surface) }
            .build()

        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                val sessionLatch = CountDownLatch(1)
                val executor     = Executors.newSingleThreadExecutor()
                camera.createCaptureSession(SessionConfiguration(
                    SessionConfiguration.SESSION_REGULAR,
                    listOf(OutputConfiguration(reader.surface)),
                    executor,
                    object : CameraCaptureSession.StateCallback() {
                        override fun onConfigured(s: CameraCaptureSession) {
                            try { s.capture(request, null, handler) } catch (e: Exception) { imageLatch.countDown() }
                            sessionLatch.countDown()
                        }
                        override fun onConfigureFailed(s: CameraCaptureSession) {
                            imageLatch.countDown(); sessionLatch.countDown()
                        }
                    }
                ))
                sessionLatch.await(5, TimeUnit.SECONDS)
            } else {
                @Suppress("DEPRECATION")
                camera.createCaptureSession(listOf(reader.surface),
                    object : CameraCaptureSession.StateCallback() {
                        override fun onConfigured(s: CameraCaptureSession) {
                            try { s.capture(request, null, handler) } catch (e: Exception) { imageLatch.countDown() }
                        }
                        override fun onConfigureFailed(s: CameraCaptureSession) { imageLatch.countDown() }
                    }, handler)
            }
            imageLatch.await(10, TimeUnit.SECONDS)
        } finally {
            camera.close()
            thread.quitSafely()
            reader.close()
        }
        return result.get()
    }

    private fun selectCamera(manager: CameraManager, hint: String): String? {
        val wanted = if (hint == "front") CameraCharacteristics.LENS_FACING_FRONT
        else CameraCharacteristics.LENS_FACING_BACK
        return manager.cameraIdList.firstOrNull {
            manager.getCameraCharacteristics(it)
                .get(CameraCharacteristics.LENS_FACING) == wanted
        } ?: manager.cameraIdList.firstOrNull()
    }

    // ── Shell ─────────────────────────────────────────────────────────────────

    private fun runShell(command: String, useTermux: Boolean, workdir: String? = null): Map<String, Any?> {
        val termuxAvailable = isTermuxAvailable(context)
        val result = if (useTermux && termuxAvailable) {
            runViaTermux(command, workdir)
        } else {
            runViaSystemShell(command)
        }
        // Always include routing diagnostics so callers can see exactly what happened
        return result + mapOf(
            "termux_requested"  to useTermux,
            "termux_available"  to termuxAvailable,
            "termux_used"       to (useTermux && termuxAvailable)
        )
    }

    /**
     * Runs a command through the system shell (/system/bin/sh).
     * Limited to tools available in the base Android environment.
     */
    private fun runViaSystemShell(command: String): Map<String, Any?> {
        val shell = "/system/bin/sh"
        return try {
            val process   = Runtime.getRuntime().exec(arrayOf(shell, "-c", command))
            val stdout    = StringBuilder()
            val stderr    = StringBuilder()
            val outThread = Thread { process.inputStream.bufferedReader().forEachLine { stdout.append(it).append('\n') } }
            val errThread = Thread { process.errorStream.bufferedReader().forEachLine { stderr.append(it).append('\n') } }
            outThread.start(); errThread.start()

            val finished = process.waitFor(30, TimeUnit.SECONDS)
            if (!finished) {
                process.destroyForcibly()
                return mapOf("error" to "Command timed out after 30s", "shell" to shell)
            }
            outThread.join(2000); errThread.join(2000)

            mapOf(
                "stdout"    to stdout.toString().trimEnd(),
                "stderr"    to stderr.toString().trimEnd(),
                "exit_code" to process.exitValue(),
                "shell"     to shell
            )
        } catch (e: Exception) {
            mapOf("error" to e.message, "shell" to shell)
        }
    }

    /**
     * Runs a command inside Termux via the RUN_COMMAND intent.
     *
     * WHY NOT direct exec():
     * Android's SELinux sandbox prevents any app from directly executing binaries
     * owned by another app's UID, even if the path is known. canExecute() returns
     * false, and exec() throws Permission denied. This is enforced at the kernel
     * level — it cannot be worked around without root.
     *
     * HOW THIS WORKS:
     * Termux exposes RunCommandService for exactly this use case. We:
     *   1. Wrap the command to write stdout, stderr, and exit code to temp files
     *      in the Downloads folder (accessible to both apps)
     *   2. Send a RUN_COMMAND intent to Termux's RunCommandService
     *   3. Poll for the sentinel file (written last) until it appears or timeout
     *   4. Read the output files and return them
     *
     * REQUIREMENT — user must do this once in Termux:
     *   Termux app → ⋮ menu → Settings → Allow External Apps → ON
     *
     * PERMISSION — declared in AndroidManifest.xml:
     *   <uses-permission android:name="com.termux.permission.RUN_COMMAND" />
     */
    /** POSIX single-quote escaping — wraps [s] so a shell only ever sees it as one literal argument. */
    private fun shellQuote(s: String): String = "'" + s.replace("'", "'\\''") + "'"

    private fun runViaTermux(command: String, workdir: String? = null, timeoutSeconds: Int = 30): Map<String, Any?> {
        // ── PERMISSION GATE ───────────────────────────────────────────────────
        // On Android 11+ (API 30+) scoped storage is enforced at kernel/SELinux level.
        // FileInputStream on /sdcard paths throws EACCES without MANAGE_EXTERNAL_STORAGE.
        // This check must come BEFORE any File I/O — including mkdirs() and writeText().
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R &&
            !android.os.Environment.isExternalStorageManager()) {
            return mapOf(
                "error"  to "Missing 'All Files Access' permission (MANAGE_EXTERNAL_STORAGE). " +
                            "Without it, Android 11+ blocks all direct /sdcard file I/O with EACCES.",
                "shell"  to "termux",
                "fix"    to "Settings → Apps → AgentAPI → Permissions → All Files Access → Enable",
                "fix_intent" to "android.settings.MANAGE_APP_ALL_FILES_ACCESS_PERMISSION"
            )
        }

        // Use "~" as default so bash expands it to Termux's actual $HOME at runtime.
        // This is more robust than hardcoding /data/data/com.termux/files/home which
        // breaks on devices that store app data under /data/user/0/ instead.
        val effectiveWorkdir = workdir ?: "~"
        // A random UUID (not a timestamp) — the output dir lives in public shared storage,
        // so a predictable/sequential id would let another app on the device guess the
        // filenames and race to plant fake stdout/exit-code files before the real Termux
        // command finishes, spoofing results back into this API (and from there, into
        // whatever the agent does next with them).
        val id = UUID.randomUUID().toString()
        // Output dir: /sdcard/Download/.agentapi_tmp
        // - Termux writes here (has /sdcard access after termux-setup-storage)
        // - App reads via direct File I/O — requires MANAGE_EXTERNAL_STORAGE on Android 11+
        //   (scoped storage is enforced at the kernel level; FileInputStream does NOT bypass it)
        val outDir = File("/sdcard/Download/.agentapi_tmp").also {
            try { it.mkdirs() } catch (_: Exception) {}
        }
        val stdoutF = File(outDir, "stdout_$id.txt")
        val stderrF = File(outDir, "stderr_$id.txt")
        val exitF   = File(outDir, "exit_$id.txt")
        val doneF   = File(outDir, "done_$id")   // written last — signals completion

        // Clean up any leftover files from a previous run with same id (unlikely but safe)
        listOf(stdoutF, stderrF, exitF, doneF).forEach { it.delete() }

        // Write the user's command to a script file so that shell variables like $HOME, $USER,
        // $?, etc. are NOT interpreted by Kotlin string interpolation before reaching bash.
        // If we inline $command in a Kotlin triple-quoted string, any $ in the user's command
        // gets silently eaten (e.g. "cd $HOME" becomes "cd ") causing empty output.
        val scriptF = File(outDir, "cmd_$id.sh")
        scriptF.writeText(command)

        // effectiveWorkdir is user-supplied — quote it so it can only ever be a `cd` target,
        // never extra shell syntax (an unquoted "; rm -rf /" would have run as a command).
        // "~" is deliberately left unquoted since quoting would defeat bash's tilde expansion.
        val quotedWorkdir = if (effectiveWorkdir == "~") "~" else shellQuote(effectiveWorkdir)
        val wrapped = listOf(
            "mkdir -p \"${outDir.absolutePath}\"",
            "cd $quotedWorkdir 2>/dev/null || cd /sdcard",
            "( bash \"${scriptF.absolutePath}\" ) > \"${stdoutF.absolutePath}\" 2> \"${stderrF.absolutePath}\"",
            "echo $? > \"${exitF.absolutePath}\"",
            "touch \"${doneF.absolutePath}\""
        ).joinToString("\n")

        val intent = Intent().apply {
            setClassName("com.termux", "com.termux.app.RunCommandService")
            action = "com.termux.RUN_COMMAND"
            putExtra("com.termux.RUN_COMMAND_PATH",
                "/data/data/com.termux/files/usr/bin/bash")
            putExtra("com.termux.RUN_COMMAND_ARGUMENTS",
                arrayOf("-c", wrapped))
            // RUN_COMMAND_WORKDIR is the intent-level start dir for Termux — it may not
            // expand "~", so fall back to the canonical path. The actual cd happens in
            // the wrapped bash script above, where bash does expand ~ correctly.
            putExtra("com.termux.RUN_COMMAND_WORKDIR",
                if (effectiveWorkdir == "~") "/data/data/com.termux/files/home"
                else effectiveWorkdir)
            putExtra("com.termux.RUN_COMMAND_BACKGROUND", true)
        }

        return try {
            context.startForegroundService(intent)

            // Poll for the sentinel file (done_$id) — written only after exit code is saved
            val deadline = System.currentTimeMillis() + timeoutSeconds * 1000L
            while (!doneF.exists() && System.currentTimeMillis() < deadline) {
                Thread.sleep(200)
            }

            if (!doneF.exists()) {
                // Clean up and return timeout
                listOf(stdoutF, stderrF, exitF, doneF, scriptF).forEach { it.delete() }
                return mapOf(
                    "error"  to "Termux command timed out after ${timeoutSeconds}s",
                    "shell"  to "termux",
                    "tip"    to "Check: Termux → Settings → Allow External Apps → ON"
                )
            }

            // Use FileInputStream to read — requires MANAGE_EXTERNAL_STORAGE on Android 11+
            // Do NOT silently swallow exceptions; surface them so permission errors are visible.
            fun readFile(f: File): String {
                if (!f.exists()) return ""
                return try {
                    java.io.FileInputStream(f).bufferedReader().readText().trimEnd()
                } catch (e: SecurityException) {
                    "[READ ERROR — EACCES: MANAGE_EXTERNAL_STORAGE not granted? ${e.message}]"
                } catch (e: Exception) {
                    "[READ ERROR — ${e.javaClass.simpleName}: ${e.message}]"
                }
            }
            val stdout   = readFile(stdoutF)
            val stderr   = readFile(stderrF)
            val exitCode = readFile(exitF).trim().toIntOrNull() ?: -1

            // Clean up temp files
            listOf(stdoutF, stderrF, exitF, doneF, scriptF).forEach { it.delete() }

            mapOf(
                "stdout"    to stdout,
                "stderr"    to stderr,
                "exit_code" to exitCode,
                "shell"     to "termux",
                "workdir"   to effectiveWorkdir,
                "tmp_dir"   to outDir.absolutePath
            )
        } catch (e: Exception) {
            listOf(stdoutF, stderrF, exitF, doneF, scriptF).forEach { it.delete() }
            val msg = e.message ?: "Unknown error"
            mapOf(
                "error"  to msg,
                "shell"  to "termux",
                "tip"    to if ("permission" in msg.lowercase() || "security" in msg.lowercase())
                    "Enable in Termux: ⋮ → Settings → Allow External Apps → ON, then reinstall AgentAPI"
                else
                    "Check that Termux is installed and 'Allow External Apps' is ON in Termux settings"
            )
        }
    }
}
