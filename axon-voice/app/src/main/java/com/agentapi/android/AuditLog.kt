package com.agentapi.android

import android.content.Context
import android.util.Log
import org.json.JSONObject
import java.io.File
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/**
 * Append-only local record of every sensitive action the agent takes (SMS sent, calls placed,
 * shell commands run, files written/deleted, config changes, approval decisions), so you can
 * reconstruct "what did it do and when" after the fact. Lives in app-private storage only —
 * never transmitted anywhere. Read back via GET /audit/log.
 */
object AuditLog {

    private const val TAG        = "AuditLog"
    private const val MAX_BYTES  = 2 * 1024 * 1024 // rotate at 2MB
    private val sdf = SimpleDateFormat("yyyy-MM-dd'T'HH:mm:ss.SSSXXX", Locale.US)

    private fun file(ctx: Context) = File(ctx.filesDir, "audit_log.jsonl")

    @Synchronized
    fun record(ctx: Context, action: String, detail: String, outcome: String) {
        try {
            val f = file(ctx)
            if (f.exists() && f.length() > MAX_BYTES) {
                f.copyTo(File(ctx.filesDir, "audit_log.jsonl.1"), overwrite = true)
                f.delete()
            }
            val entry = JSONObject()
                .put("time", sdf.format(Date()))
                .put("action", action)
                .put("detail", detail)
                .put("outcome", outcome)
            f.appendText(entry.toString() + "\n")
        } catch (e: Exception) {
            Log.w(TAG, "Failed to write audit entry: ${e.message}")
        }
    }

    /** Most recent [lines] entries, oldest first. */
    fun tail(ctx: Context, lines: Int = 200): List<String> {
        val f = file(ctx)
        if (!f.exists()) return emptyList()
        return f.readLines().takeLast(lines)
    }
}
