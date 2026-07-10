<script setup>
import { onMounted, ref } from 'vue'

const props = defineProps({
  placeholder: { type: String, default: 'Search…' },
  // Focused on mount so the user can type immediately. Pages where another
  // control owns first focus (e.g. the chat composer) pass :autofocus="false".
  autofocus: { type: Boolean, default: true },
})

const model = defineModel({ type: String, default: '' })
const inputRef = ref(null)

onMounted(() => {
  if (props.autofocus) inputRef.value?.focus()
})

defineExpose({ focus: () => inputRef.value?.focus() })
</script>

<template>
  <div class="search-field">
    <svg
      class="search-field-icon"
      viewBox="0 0 24 24"
      width="14"
      height="14"
      aria-hidden="true"
    >
      <circle
        cx="11"
        cy="11"
        r="7"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
      />
      <path
        d="m16.5 16.5 4.5 4.5"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
      />
    </svg>
    <input
      ref="inputRef"
      v-model="model"
      type="search"
      class="premium-input"
      :placeholder="placeholder"
    >
  </div>
</template>
