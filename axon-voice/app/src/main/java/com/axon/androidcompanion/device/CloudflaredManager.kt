package com.axon.androidcompanion.device

import android.content.Context
import android.util.Log
import java.io.File
import java.util.concurrent.atomic.AtomicInteger

/**
 * Launches and supervises an embedded `cloudflared` binary so the phone doesn't need
 * Termux (or any other app) running just to stay reachable through the Cloudflare Tunnel.
 *
 * The binary ships inside the APK as a native library —
 * app/src/main/jniLibs/<abi>/libcloudflared.so — so Android extracts it to a real,
 * executable file at install time. That requires:
 *   - build.gradle: packagingOptions.jniLibs.useLegacyPackaging = true
 *   - AndroidManifest.xml: android:extractNativeLibs="true"
 * (see fetch_cloudflared.sh for how to populate jniLibs/ — cloudflared's binaries
 * can't be downloaded from inside this build environment, see that script's header)
 *
 * Uses cloudflared's token-based "remotely-managed tunnel" mode (`tunnel run` with
 * TUNNEL_TOKEN set) — the tunnel and its ingress rule (→ http://127.0.0.1:PORT) are
 * configured once in the Cloudflare Zero Trust dashboard; the phone only ever needs the
 * connector token, which can be changed anytime from Settings or POST/PATCH /config
 * without touching the dashboard again.
 */
object CloudflaredManager {

    private const val TAG           = "CloudflaredManager"
    private const val LOG_MAX_BYTES = 512 * 1024L

    @Volatile private var process: Process? = null

    /**
     * Which supervisor generation is allowed to run. Every [start] and [stop]
     * bumps it, and a supervisor exits as soon as its own generation goes stale.
     *
     * A plain "stopping" flag could not express this: [start] calls [stop] and
     * then immediately clears the flag for the incoming supervisor, so an old
     * one that was parked in `waitFor()` or its backoff sleep woke up, read the
     * freshly-cleared flag, and carried on — launching a second cloudflared
     * against the same token and overwriting [process], which orphaned one
     * handle beyond the reach of [stop]. Since the watchdog restarts ApiService
     * (and with it the tunnel) whenever an OEM killer takes it, those duplicates
     * accumulated until the connector churn took the tunnel down at Cloudflare's
     * edge. A generation is per-supervisor, so it cannot be reset out from under
     * the thread it applies to.
     */
    private val generation = AtomicInteger(0)

    fun isRunning(): Boolean = process?.isAlive == true

    fun binaryPath(ctx: Context): String? {
        val path = File(ctx.applicationInfo.nativeLibraryDir, "libcloudflared.so")
        return if (path.exists() && path.canExecute()) path.absolutePath else null
    }

    /** (Re)starts the tunnel using the token currently in AppConfig. No-op if no token is set. */
    @Synchronized
    fun start(ctx: Context) {
        stop() // clean restart if one is already running (e.g. token just changed)

        if (AppConfig.getCloudflaredToken(ctx).isBlank()) {
            Log.i(TAG, "No cloudflared token configured — tunnel not started")
            return
        }
        val binary = binaryPath(ctx)
        if (binary == null) {
            Log.w(TAG, "cloudflared binary missing for this device's ABI at " +
                "${ctx.applicationInfo.nativeLibraryDir} — see fetch_cloudflared.sh")
            return
        }

        val gen = generation.incrementAndGet()
        Thread({ supervise(ctx, binary, gen) }, "CloudflaredSupervisor-$gen").apply {
            isDaemon = true
            start()
        }
    }

    @Synchronized
    fun stop() {
        generation.incrementAndGet() // retires every live supervisor
        process?.destroy()
        process = null
    }

    private fun supervise(ctx: Context, binary: String, gen: Int) {
        var backoffMs = 3_000L
        while (gen == generation.get()) {
            val token = AppConfig.getCloudflaredToken(ctx)
            if (token.isBlank()) return

            try {
                val p = ProcessBuilder(binary, "tunnel", "--no-autoupdate", "run")
                    .redirectErrorStream(true)
                    .apply { environment()["TUNNEL_TOKEN"] = token }
                    .start()
                // Publish the handle only while still current, under the same
                // lock stop() takes — otherwise a stop() landing during this
                // start would destroy the previous process and then be
                // overwritten by this one, leaving it running and unkillable.
                val owned = synchronized(this) {
                    if (gen == generation.get()) {
                        process = p
                        true
                    } else {
                        false
                    }
                }
                if (!owned) {
                    p.destroy()
                    return
                }
                Log.i(TAG, "cloudflared started")
                backoffMs = 3_000L // reset backoff after a successful start

                streamToLog(ctx, p)
                val exitCode = p.waitFor()
                Log.w(TAG, "cloudflared exited with code $exitCode")
            } catch (e: Exception) {
                Log.e(TAG, "Failed to launch cloudflared: ${e.message}", e)
            }

            if (gen != generation.get()) return
            Thread.sleep(backoffMs)
            backoffMs = (backoffMs * 2).coerceAtMost(60_000L)
        }
    }

    private fun streamToLog(ctx: Context, process: Process) {
        val logFile = File(ctx.filesDir, "cloudflared.log")
        Thread({
            try {
                process.inputStream.bufferedReader().forEachLine { line ->
                    if (logFile.exists() && logFile.length() > LOG_MAX_BYTES) logFile.delete()
                    logFile.appendText(line + "\n")
                }
            } catch (_: Exception) {}
        }, "CloudflaredLogReader").apply { isDaemon = true; start() }
    }

    fun tailLog(ctx: Context, lines: Int = 80): List<String> {
        val f = File(ctx.filesDir, "cloudflared.log")
        return if (f.exists()) f.readLines().takeLast(lines) else emptyList()
    }
}
