package com.axon.androidcompanion.device

import android.content.Context
import android.util.Base64
import android.util.Log
import java.io.File
import java.net.InetAddress
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

    /**
     * The tunnel log lives in the app's *external* files dir so it can be pulled
     * off the device (adb, a file manager) without root or a debuggable build.
     * The API server is loopback-only, so when the tunnel is down there is no
     * remote way to ask the phone what went wrong — and a release APK blocks
     * `run-as`, which left this log completely unreadable exactly when it
     * mattered. Falls back to internal storage if external is unavailable.
     */
    private fun logFile(ctx: Context): File =
        File(ctx.getExternalFilesDir(null) ?: ctx.filesDir, "cloudflared.log")

    /**
     * A connector token is base64 of `{"a":account,"t":tunnelID,"s":secret}`.
     * cloudflared validates that shape locally and, if it fails, prints
     * "Provided Tunnel token is not valid." and exits immediately — which from
     * outside is indistinguishable from any other crash loop. Checking the same
     * thing at the point of entry turns a silent 3-second restart cycle into an
     * error message next to the paste field.
     */
    fun tokenLooksValid(token: String): Boolean {
        val t = token.trim()
        if (t.isEmpty()) return false
        for (flags in intArrayOf(Base64.DEFAULT, Base64.URL_SAFE)) {
            try {
                val json = String(Base64.decode(t, flags), Charsets.UTF_8)
                val o = org.json.JSONObject(json)
                if (o.has("a") && o.has("t") && o.has("s")) return true
            } catch (_: Exception) {
                // try the next alphabet
            }
        }
        return false
    }

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

    /**
     * Cloudflare edge addresses, resolved through Android's resolver.
     *
     * The bundled binary is a **linux/arm64** build (it logs `GOOS: linux`), so
     * Go's pure-Go resolver expects `/etc/resolv.conf` — a file Android does not
     * have. With nothing to read it falls back to `127.0.0.1:53`/`[::1]:53`,
     * nothing is listening there, and every SRV lookup for
     * `_v2-origintunneld._tcp.argotunnel.com` dies with "connection refused".
     * cloudflared then treats edge discovery as fatal and exits, which is what
     * produced the endless 3-second restart loop.
     *
     * Java's InetAddress goes through Android's own resolver (netd) and works
     * normally, so we do the lookup here and hand the results over as `--edge`.
     * Verified on-device: with `--edge` supplied, cloudflared connects to the
     * edge and the remaining SRV noise in its log is non-fatal.
     */
    private fun edgeAddrs(): List<String> = try {
        listOf("region1.v2.argotunnel.com", "region2.v2.argotunnel.com")
            .flatMap { host ->
                try { InetAddress.getAllByName(host).toList() } catch (_: Exception) { emptyList() }
            }
            .mapNotNull { it.hostAddress }
            // An IPv6 literal has to be bracketed before a :port is appended, or
            // cloudflared reads the whole thing as "too many colons".
            .map { if (it.contains(':')) "[$it]:7844" else "$it:7844" }
            .distinct()
    } catch (_: Exception) {
        emptyList()
    }

    private fun supervise(ctx: Context, binary: String, gen: Int) {
        var backoffMs = 3_000L
        while (gen == generation.get()) {
            val token = AppConfig.getCloudflaredToken(ctx)
            if (token.isBlank()) return

            try {
                // Resolve the edge here rather than letting cloudflared do it:
                // see [edgeAddrs]. Re-resolved every attempt so a rotated edge
                // IP is picked up on the next restart rather than pinned.
                val edges = edgeAddrs()
                if (edges.isEmpty()) {
                    appendLog(ctx, "--- supervisor $gen: edge DNS lookup failed, " +
                        "starting without --edge (cloudflared will likely fail) ---")
                }
                val cmd = mutableListOf(binary, "tunnel", "--no-autoupdate")
                edges.forEach { cmd += listOf("--edge", it) }
                cmd += "run"

                val p = ProcessBuilder(cmd)
                    .redirectErrorStream(true)
                    .apply {
                        environment()["TUNNEL_TOKEN"] = token
                        // cloudflared is a Go binary expecting a normal user
                        // environment. In an Android sandbox HOME is unset or
                        // points somewhere unwritable, so its attempts to place
                        // ~/.cloudflared state fail — pointing both at our own
                        // private dirs costs nothing and removes that class of
                        // startup death.
                        environment()["HOME"] = ctx.filesDir.absolutePath
                        environment()["TMPDIR"] = ctx.cacheDir.absolutePath
                    }
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
                appendLog(ctx, "--- supervisor $gen: cloudflared started ---")
                backoffMs = 3_000L // reset backoff after a successful start

                streamToLog(ctx, p)
                val exitCode = p.waitFor()
                Log.w(TAG, "cloudflared exited with code $exitCode")
                // Into the file, not just logcat: the API server is
                // loopback-only, so when the tunnel is down this file read from
                // Settings is the only way to see why without a USB cable.
                appendLog(ctx, "--- supervisor $gen: cloudflared exited with code $exitCode ---")
            } catch (e: Exception) {
                Log.e(TAG, "Failed to launch cloudflared: ${e.message}", e)
                appendLog(ctx, "--- supervisor $gen: failed to launch — ${e.message} ---")
            }

            if (gen != generation.get()) return
            Thread.sleep(backoffMs)
            backoffMs = (backoffMs * 2).coerceAtMost(60_000L)
        }
    }

    /** Timestamped supervisor line in the same file cloudflared's own output
     *  goes to, so the two interleave into one readable history. */
    @Synchronized
    private fun appendLog(ctx: Context, line: String) {
        try {
            val stamp = java.text.SimpleDateFormat("MM-dd HH:mm:ss", java.util.Locale.US)
                .format(java.util.Date())
            logFile(ctx).appendText("$stamp $line\n")
        } catch (_: Exception) {}
    }

    private fun streamToLog(ctx: Context, process: Process) {
        val logFile = logFile(ctx)
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
        val f = logFile(ctx)
        return if (f.exists()) f.readLines().takeLast(lines) else emptyList()
    }
}
