package com.axon.voice.ui

import android.content.Intent
import android.os.Bundle
import android.view.View
import android.widget.ImageButton
import android.widget.TextView
import androidx.appcompat.app.AlertDialog
import androidx.appcompat.app.AppCompatActivity
import androidx.recyclerview.widget.LinearLayoutManager
import androidx.recyclerview.widget.RecyclerView
import com.axon.voice.R

/**
 * Browse past "Hey Axon" conversations. Each wake mints its own session id
 * ([com.axon.voice.Prefs.newWakeConversationId]) and is saved on its own —
 * unlike the single ongoing chat thread (always one tap away by opening the
 * app), these have no other way back once the moment passes, so this is the
 * only place to review what was actually said hands-free.
 */
class HistoryActivity : AppCompatActivity() {

    private lateinit var adapter: ConversationAdapter
    private lateinit var emptyState: TextView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_history)

        emptyState = findViewById(R.id.emptyState)
        val list = findViewById<RecyclerView>(R.id.historyList)
        list.layoutManager = LinearLayoutManager(this)
        adapter = ConversationAdapter(
            onOpen = { summary ->
                startActivity(
                    Intent(this, ConversationDetailActivity::class.java)
                        .putExtra(ConversationDetailActivity.EXTRA_SESSION_ID, summary.sessionId)
                        .putExtra(ConversationDetailActivity.EXTRA_UPDATED_AT, summary.updatedAt)
                )
            },
            onDelete = { summary -> confirmDelete(summary) },
        )
        list.adapter = adapter

        findViewById<ImageButton>(R.id.backBtn).setOnClickListener { finish() }
    }

    override fun onResume() {
        super.onResume()
        // Reload every time this screen becomes visible — covers a deletion
        // made from ConversationDetailActivity and any wake that landed while
        // this screen was backgrounded.
        refresh()
    }

    private fun refresh() {
        val items = ChatHistory.listWakeConversations(this)
        adapter.submit(items)
        emptyState.visibility = if (items.isEmpty()) View.VISIBLE else View.GONE
    }

    private fun confirmDelete(summary: ChatHistory.Summary) {
        AlertDialog.Builder(this)
            .setMessage(R.string.delete_conversation_confirm)
            .setPositiveButton(R.string.delete_conversation) { _, _ ->
                ChatHistory.delete(this, summary.sessionId)
                refresh()
            }
            .setNegativeButton(android.R.string.cancel, null)
            .show()
    }
}
