package com.axon.voice.ui

import android.annotation.SuppressLint
import android.view.Gravity
import android.view.LayoutInflater
import android.view.View
import android.view.ViewGroup
import android.widget.LinearLayout
import android.widget.TextView
import androidx.core.content.ContextCompat
import androidx.recyclerview.widget.RecyclerView
import com.axon.voice.R

class TranscriptAdapter : RecyclerView.Adapter<TranscriptAdapter.VH>() {

    data class Msg(val role: String, var text: String)

    private val items = mutableListOf<Msg>()

    val lastRole: String? get() = items.lastOrNull()?.role
    val lastText: String get() = items.lastOrNull()?.text ?: ""

    /** Index of the most recently added message — pin a streaming bubble to
     *  this instead of "the last item", which can shift under it when the
     *  wake service inserts an exchange mid-stream. */
    val lastIndex: Int get() = items.size - 1

    fun textAt(index: Int): String = items.getOrNull(index)?.text ?: ""

    fun appendAt(index: Int, text: String) {
        val m = items.getOrNull(index) ?: return
        m.text += text
        notifyItemChanged(index)
    }

    fun setAt(index: Int, text: String) {
        val m = items.getOrNull(index) ?: return
        m.text = text
        notifyItemChanged(index)
    }

    fun add(role: String, text: String) {
        items.add(Msg(role, text))
        notifyItemInserted(items.size - 1)
    }

    fun appendToLast(text: String) {
        if (items.isEmpty()) return
        items.last().text += text
        notifyItemChanged(items.size - 1)
    }

    fun setLast(text: String) {
        if (items.isEmpty()) return
        items.last().text = text
        notifyItemChanged(items.size - 1)
    }

    fun clear() {
        val n = items.size
        if (n == 0) return
        items.clear()
        notifyItemRangeRemoved(0, n)
    }

    /** Copy of the current transcript, for persistence (Chat page history). */
    fun snapshot(): List<Msg> = items.map { it.copy() }

    @SuppressLint("NotifyDataSetChanged")
    fun load(msgs: List<Msg>) {
        items.clear()
        items.addAll(msgs)
        notifyDataSetChanged()
    }

    class VH(v: View) : RecyclerView.ViewHolder(v) {
        val row: LinearLayout = v.findViewById(R.id.row)
        val msg: TextView = v.findViewById(R.id.msg)
    }

    override fun onCreateViewHolder(parent: ViewGroup, viewType: Int): VH =
        VH(LayoutInflater.from(parent.context).inflate(R.layout.item_transcript, parent, false))

    override fun getItemCount(): Int = items.size

    override fun onBindViewHolder(holder: VH, position: Int) {
        val item = items[position]
        val ctx = holder.itemView.context
        holder.msg.text = item.text
        holder.row.gravity = if (item.role == "user") Gravity.END else Gravity.START
        val bubbleColor = if (item.role == "user") R.color.user_bubble else R.color.surface
        holder.msg.background.mutate().setTint(ContextCompat.getColor(ctx, bubbleColor))
        val textColor = if (item.role == "error") R.color.error else R.color.text
        holder.msg.setTextColor(ContextCompat.getColor(ctx, textColor))
    }
}
