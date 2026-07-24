package com.axon.voice.ui

import android.annotation.SuppressLint
import android.text.format.DateUtils
import android.view.LayoutInflater
import android.view.View
import android.view.ViewGroup
import android.widget.ImageButton
import android.widget.TextView
import androidx.recyclerview.widget.RecyclerView
import com.axon.voice.R

/**
 * One row per saved "Hey Axon" conversation in the History list: a preview of
 * what was said, how long ago, and a delete action. Tapping the row (not the
 * delete button) opens the full transcript.
 */
class ConversationAdapter(
    private val onOpen: (ChatHistory.Summary) -> Unit,
    private val onDelete: (ChatHistory.Summary) -> Unit,
) : RecyclerView.Adapter<ConversationAdapter.VH>() {

    private val items = mutableListOf<ChatHistory.Summary>()

    @SuppressLint("NotifyDataSetChanged")
    fun submit(list: List<ChatHistory.Summary>) {
        items.clear()
        items.addAll(list)
        notifyDataSetChanged()
    }

    class VH(v: View) : RecyclerView.ViewHolder(v) {
        val preview: TextView = v.findViewById(R.id.convPreview)
        val time: TextView = v.findViewById(R.id.convTime)
        val delete: ImageButton = v.findViewById(R.id.convDelete)
    }

    override fun onCreateViewHolder(parent: ViewGroup, viewType: Int): VH =
        VH(LayoutInflater.from(parent.context).inflate(R.layout.item_conversation, parent, false))

    override fun getItemCount(): Int = items.size

    override fun onBindViewHolder(holder: VH, position: Int) {
        val item = items[position]
        holder.preview.text = item.preview
        holder.time.text = DateUtils.getRelativeTimeSpanString(
            item.updatedAt, System.currentTimeMillis(), DateUtils.MINUTE_IN_MILLIS
        )
        holder.itemView.setOnClickListener { onOpen(item) }
        holder.delete.setOnClickListener { onDelete(item) }
    }
}
