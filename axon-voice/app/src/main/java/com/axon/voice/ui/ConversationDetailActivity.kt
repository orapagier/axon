package com.axon.voice.ui

import android.os.Bundle
import android.text.format.DateUtils
import android.widget.ImageButton
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import androidx.recyclerview.widget.LinearLayoutManager
import androidx.recyclerview.widget.RecyclerView
import com.axon.voice.R

/** Read-only transcript of one past "Hey Axon" conversation, opened from
 *  [HistoryActivity]. No input row — reviewing only; continuing the
 *  conversation happens by saying "Hey Axon" again, which starts a new one. */
class ConversationDetailActivity : AppCompatActivity() {

    companion object {
        const val EXTRA_SESSION_ID = "session_id"
        const val EXTRA_UPDATED_AT = "updated_at"
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_conversation_detail)

        val sessionId = intent.getStringExtra(EXTRA_SESSION_ID)
        if (sessionId == null) {
            finish()
            return
        }

        findViewById<ImageButton>(R.id.backBtn).setOnClickListener { finish() }

        val updatedAt = intent.getLongExtra(EXTRA_UPDATED_AT, 0L)
        if (updatedAt > 0) {
            findViewById<TextView>(R.id.detailTime).text = DateUtils.getRelativeTimeSpanString(
                updatedAt, System.currentTimeMillis(), DateUtils.MINUTE_IN_MILLIS
            )
        }

        val list = findViewById<RecyclerView>(R.id.detailList)
        list.layoutManager = LinearLayoutManager(this).apply { stackFromEnd = true }
        val adapter = TranscriptAdapter()
        list.adapter = adapter
        adapter.load(ChatHistory.load(this, sessionId))
    }
}
