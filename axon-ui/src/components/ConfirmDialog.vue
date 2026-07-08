<script setup>
import Modal from './Modal.vue'
import { confirmState } from '../lib/confirm.js'

function respond(value) {
  const state = confirmState.value
  confirmState.value = null
  state?.resolve(value)
}

function onModalChange(open) {
  if (!open) respond(false)
}
</script>

<template>
  <Modal
    :model-value="!!confirmState"
    :title="confirmState?.title"
    max-width="420px"
    @update:model-value="onModalChange"
  >
    <template v-if="confirmState">
      <p class="confirm-message">
        {{ confirmState.message }}
      </p>
      <div class="modal-actions">
        <button
          class="btn btn-ghost"
          type="button"
          @click="respond(false)"
        >
          {{ confirmState.cancelText }}
        </button>
        <button
          :class="['btn', confirmState.danger ? 'btn-danger' : 'btn-primary']"
          type="button"
          autofocus
          @click="respond(true)"
        >
          {{ confirmState.confirmText }}
        </button>
      </div>
    </template>
  </Modal>
</template>

<style scoped>
.confirm-message {
  color: var(--muted);
  font-size: 14px;
  line-height: 1.55;
  margin: 0;
}
</style>
