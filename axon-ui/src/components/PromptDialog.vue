<script setup>
import { nextTick, ref, watch } from 'vue'
import Modal from './Modal.vue'
import { promptState } from '../lib/prompt.js'

const inputEl = ref(null)

function respond(value) {
  const state = promptState.value
  promptState.value = null
  state?.resolve(value)
}

function onModalChange(open) {
  if (!open) respond(null)
}

function submit() {
  respond(promptState.value?.value ?? '')
}

watch(
  () => !!promptState.value,
  async (open) => {
    if (!open) return
    await nextTick()
    inputEl.value?.focus()
    inputEl.value?.select()
  }
)
</script>

<template>
  <Modal
    :model-value="!!promptState"
    :title="promptState?.title"
    max-width="420px"
    @update:model-value="onModalChange"
  >
    <template v-if="promptState">
      <p
        v-if="promptState.message"
        class="prompt-message"
      >
        {{ promptState.message }}
      </p>
      <input
        ref="inputEl"
        v-model="promptState.value"
        type="text"
        class="premium-input"
        :placeholder="promptState.placeholder"
        @keydown.enter="submit"
      >
      <div class="modal-actions">
        <button
          class="btn btn-ghost"
          type="button"
          @click="respond(null)"
        >
          {{ promptState.cancelText }}
        </button>
        <button
          class="btn btn-primary"
          type="button"
          @click="submit"
        >
          {{ promptState.confirmText }}
        </button>
      </div>
    </template>
  </Modal>
</template>

<style scoped>
.prompt-message {
  color: var(--muted);
  font-size: 13px;
  line-height: 1.55;
  margin: 0 0 12px;
}
</style>
