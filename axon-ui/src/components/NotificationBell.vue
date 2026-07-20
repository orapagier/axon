<script setup>
import { ref, computed, onMounted, onUnmounted } from 'vue'
import Modal from './Modal.vue'
import { confirmDialog } from '../lib/confirm.js'
import {
  notifications,
  markRead,
  markAllRead,
  deleteNotification,
  clearAllNotifications,
} from '../lib/notifications.js'

const open = ref(false)
const activeNotification = ref(null)
const rootEl = ref(null)

const unreadCount = computed(() => notifications.value.filter((n) => !n.read).length)
const badgeLabel = computed(() => (unreadCount.value > 99 ? '99+' : String(unreadCount.value)))

function toggleOpen() {
  open.value = !open.value
}

function onOutsideClick(e) {
  if (open.value && rootEl.value && !rootEl.value.contains(e.target)) {
    open.value = false
  }
}

function onKeydown(e) {
  if (e.key === 'Escape') open.value = false
}

onMounted(() => {
  window.addEventListener('click', onOutsideClick, true)
  window.addEventListener('keydown', onKeydown)
})
onUnmounted(() => {
  window.removeEventListener('click', onOutsideClick, true)
  window.removeEventListener('keydown', onKeydown)
})

function openNotification(n) {
  activeNotification.value = n
  markRead(n.key)
}

function closeDetail(v) {
  if (!v) activeNotification.value = null
}

function removeOne(e, key) {
  e.stopPropagation()
  deleteNotification(key)
}

async function clearAll() {
  const ok = await confirmDialog('This will permanently delete all notifications.', {
    title: 'Clear notifications',
    confirmText: 'Clear all',
    danger: true,
  })
  if (!ok) return
  clearAllNotifications()
}

function timeAgo(ts) {
  const diff = Math.max(0, Date.now() - ts)
  const min = Math.floor(diff / 60000)
  if (min < 1) return 'just now'
  if (min < 60) return `${min}m ago`
  const hr = Math.floor(min / 60)
  if (hr < 24) return `${hr}h ago`
  const day = Math.floor(hr / 24)
  if (day < 7) return `${day}d ago`
  return new Date(ts).toLocaleDateString()
}

function fullTime(ts) {
  return new Date(ts).toLocaleString()
}
</script>

<template>
  <div
    ref="rootEl"
    class="notif-bell"
  >
    <button
      class="shell-icon-btn notif-trigger"
      type="button"
      title="Notifications"
      @click="toggleOpen"
    >
      <svg
        viewBox="0 0 24 24"
        aria-hidden="true"
      >
        <path
          d="M6 10a6 6 0 1 1 12 0c0 3.4 1 5.2 1.8 6.2.3.4 0 1-.5 1H4.7c-.5 0-.8-.6-.5-1C5 15.2 6 13.4 6 10Z"
          fill="none"
          stroke="currentColor"
          stroke-width="1.8"
          stroke-linecap="round"
          stroke-linejoin="round"
        />
        <path
          d="M9.5 19.5a2.5 2.5 0 0 0 5 0"
          fill="none"
          stroke="currentColor"
          stroke-width="1.8"
          stroke-linecap="round"
        />
      </svg>
      <span
        v-if="unreadCount > 0"
        class="notif-badge"
      >{{ badgeLabel }}</span>
    </button>

    <Transition name="notif-pop">
      <div
        v-if="open"
        class="notif-panel"
        role="menu"
      >
        <div class="notif-panel-header">
          <span class="notif-panel-title">Notifications</span>
          <div class="notif-panel-actions">
            <button
              class="notif-link-btn"
              type="button"
              :disabled="unreadCount === 0"
              @click="markAllRead"
            >
              Mark all read
            </button>
            <button
              class="notif-link-btn notif-link-danger"
              type="button"
              :disabled="notifications.length === 0"
              @click="clearAll"
            >
              Clear
            </button>
          </div>
        </div>

        <div
          v-if="notifications.length === 0"
          class="notif-empty"
        >
          No notifications yet.
        </div>

        <div
          v-else
          class="notif-list"
        >
          <div
            v-for="n in notifications"
            :key="n.key"
            class="notif-row"
            :class="{ unread: !n.read, error: !n.ok }"
            @click="openNotification(n)"
          >
            <span
              class="notif-dot"
              aria-hidden="true"
            />
            <div class="notif-row-body">
              <p class="notif-row-msg">
                {{ n.msg }}
              </p>
              <span class="notif-row-time">{{ timeAgo(n.ts) }}</span>
            </div>
            <button
              class="notif-row-delete"
              type="button"
              title="Delete"
              @click="removeOne($event, n.key)"
            >
              <svg
                viewBox="0 0 24 24"
                aria-hidden="true"
              >
                <path
                  d="M18 6 6 18M6 6l12 12"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="2"
                  stroke-linecap="round"
                />
              </svg>
            </button>
          </div>
        </div>
      </div>
    </Transition>

    <Modal
      :model-value="!!activeNotification"
      title="Notification"
      max-width="480px"
      @update:model-value="closeDetail"
    >
      <template v-if="activeNotification">
        <p class="notif-detail-time">
          {{ fullTime(activeNotification.ts) }}
        </p>
        <p
          class="notif-detail-msg"
          :class="{ error: !activeNotification.ok }"
        >
          {{ activeNotification.msg }}
        </p>
      </template>
    </Modal>
  </div>
</template>

<style scoped>
.notif-bell {
  position: relative;
}

.notif-trigger {
  position: relative;
}

.notif-badge {
  position: absolute;
  top: -4px;
  right: -4px;
  min-width: 16px;
  height: 16px;
  padding: 0 4px;
  border-radius: 999px;
  background: var(--red);
  color: #2a0d10;
  font-size: 0.62rem;
  font-weight: 700;
  line-height: 16px;
  text-align: center;
}

.notif-panel {
  position: absolute;
  top: calc(100% + 10px);
  right: 0;
  width: 340px;
  max-width: calc(100vw - 32px);
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: 12px;
  box-shadow: var(--shadow-lg);
  z-index: 1000;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.notif-panel-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 10px 12px;
  border-bottom: 1px solid var(--border);
}

.notif-panel-title {
  font-size: 0.8rem;
  font-weight: 700;
  color: var(--text);
}

.notif-panel-actions {
  display: flex;
  gap: 10px;
}

.notif-link-btn {
  background: none;
  border: none;
  color: var(--accent);
  font-size: 0.7rem;
  font-weight: 600;
  cursor: pointer;
  padding: 0;
}

.notif-link-btn:hover:not(:disabled) {
  text-decoration: underline;
}

.notif-link-btn:disabled {
  color: var(--muted);
  cursor: default;
  opacity: 0.6;
}

.notif-link-danger {
  color: var(--red);
}

.notif-empty {
  padding: 24px 16px;
  text-align: center;
  color: var(--muted);
  font-size: 0.78rem;
}

.notif-list {
  max-height: 360px;
  overflow-y: auto;
}

.notif-row {
  display: flex;
  align-items: flex-start;
  gap: 8px;
  padding: 10px 12px;
  border-bottom: 1px solid var(--border);
  cursor: pointer;
  transition: background 0.15s ease;
}

.notif-row:last-child {
  border-bottom: none;
}

.notif-row:hover {
  background: var(--surface-muted-strong);
}

.notif-dot {
  flex-shrink: 0;
  width: 7px;
  height: 7px;
  border-radius: 999px;
  margin-top: 6px;
  background: transparent;
}

.notif-row.unread .notif-dot {
  background: var(--accent);
}

.notif-row.error.unread .notif-dot {
  background: var(--red);
}

.notif-row-body {
  flex: 1;
  min-width: 0;
}

.notif-row-msg {
  font-size: 0.78rem;
  color: var(--text);
  line-height: 1.4;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
}

.notif-row.error .notif-row-msg {
  color: var(--red);
}

.notif-row-time {
  display: block;
  margin-top: 4px;
  font-size: 0.66rem;
  color: var(--muted);
}

.notif-row-delete {
  flex-shrink: 0;
  width: 20px;
  height: 20px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  padding: 0;
  border: none;
  border-radius: 6px;
  background: none;
  color: var(--muted);
  cursor: pointer;
  opacity: 0;
  transition: opacity 0.15s ease, background 0.15s ease, color 0.15s ease;
}

.notif-row:hover .notif-row-delete {
  opacity: 1;
}

.notif-row-delete:hover {
  background: var(--redDim);
  color: var(--red);
}

.notif-row-delete svg {
  width: 12px;
  height: 12px;
}

.notif-detail-time {
  color: var(--muted);
  font-size: 0.72rem;
  margin: 0 0 8px;
}

.notif-detail-msg {
  color: var(--text);
  font-size: 0.85rem;
  line-height: 1.55;
  margin: 0;
  white-space: pre-wrap;
  word-break: break-word;
}

.notif-detail-msg.error {
  color: var(--red);
}

.notif-pop-enter-active,
.notif-pop-leave-active {
  transition: opacity 0.15s ease, transform 0.15s ease;
}

.notif-pop-enter-from,
.notif-pop-leave-to {
  opacity: 0;
  transform: translateY(-6px);
}
</style>
