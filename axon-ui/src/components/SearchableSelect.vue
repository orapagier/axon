<script setup>
import { ref, watch, computed } from 'vue'

const props = defineProps({
  modelValue: { type: [String, Number], default: '' },
  options: { type: Array, default: () => [] },
  placeholder: { type: String, default: 'Search...' },
  allowCustomValue: { type: Boolean, default: false },
})

const emit = defineEmits(['update:modelValue'])

const isOpen = ref(false)
const searchTerm = ref('')
const inputRef = ref(null)

const selectedOption = computed(() => props.options.find((o) => o.value === props.modelValue))

const selectedName = computed(() => {
  const opt = selectedOption.value
  return opt ? opt.name : props.modelValue || ''
})

watch(
  () => props.modelValue,
  () => {
    if (!isOpen.value) {
      searchTerm.value = selectedName.value
    }
  },
  { immediate: true }
)

watch(() => props.options, () => {
  if (!isOpen.value) {
    searchTerm.value = selectedName.value
  }
})

const filteredOptions = computed(() => {
  if (!searchTerm.value || !isOpen.value) return props.options
  const term = searchTerm.value.toLowerCase()
  return props.options.filter((o) => {
    const name = String(o.name || '').toLowerCase()
    const value = String(o.value || '').toLowerCase()
    const description = String(o.description || '').toLowerCase()
    return name.includes(term) || value.includes(term) || description.includes(term)
  })
})

function open() {
  isOpen.value = true
  searchTerm.value = ''
}

function findMatchingOption(rawTerm) {
  const term = String(rawTerm || '').trim().toLowerCase()
  if (!term) return null
  return (
    props.options.find((o) => {
      const name = String(o.name || '').trim().toLowerCase()
      const value = String(o.value || '').trim().toLowerCase()
      return name === term || value === term
    }) || null
  )
}

function commitCustomIfNeeded() {
  if (!props.allowCustomValue) return

  const typed = String(searchTerm.value || '').trim()
  if (!typed) {
    emit('update:modelValue', '')
    return
  }

  const matched = findMatchingOption(typed)
  if (matched) {
    emit('update:modelValue', matched.value)
    searchTerm.value = matched.name
    return
  }

  emit('update:modelValue', typed)
}

function close() {
  setTimeout(() => {
    commitCustomIfNeeded()
    isOpen.value = false
    searchTerm.value = selectedName.value
  }, 200)
}

function selectOption(val, name) {
  searchTerm.value = name
  isOpen.value = false
  inputRef.value?.blur()
  emit('update:modelValue', val)
}

function onEnter() {
  if (!isOpen.value) {
    open()
    return
  }

  const matched = findMatchingOption(searchTerm.value)
  if (matched) {
    selectOption(matched.value, matched.name)
    return
  }

  if (props.allowCustomValue) {
    commitCustomIfNeeded()
    isOpen.value = false
    inputRef.value?.blur()
  }
}
</script>

<template>
  <div class="searchable-select">
    <input
      ref="inputRef"
      type="text"
      v-model="searchTerm"
      :placeholder="placeholder"
      @focus="open"
      @blur="close"
      @keydown.enter.prevent="onEnter"
      class="ss-input"
    />
    <span class="ss-arrow">▼</span>

    <div v-if="isOpen" class="ss-dropdown">
      <div v-if="filteredOptions.length === 0" class="ss-no-results">No matches found</div>
      <div
        v-for="opt in filteredOptions"
        :key="opt.value"
        class="ss-option"
        :class="{ 'ss-selected': opt.value === modelValue }"
        @mousedown.prevent="selectOption(opt.value, opt.name)"
      >
        <div class="ss-option-main">{{ opt.name }}</div>
        <div v-if="opt.description" class="ss-option-description">{{ opt.description }}</div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.searchable-select {
  position: relative;
  width: 100%;
}
.ss-input {
  width: 100%;
  background: rgba(255, 255, 255, 0.04);
  border: 1px solid rgba(255, 255, 255, 0.1);
  color: #f2f7ff;
  padding: 8px 30px 8px 12px;
  border-radius: 6px;
  font-size: 13px;
  outline: none;
  cursor: text;
  box-sizing: border-box;
}
.ss-input:focus {
  border-color: #6366f1;
  background: rgba(255, 255, 255, 0.06);
  box-shadow: 0 0 0 3px rgba(99, 102, 241, 0.15);
}
.ss-arrow {
  position: absolute;
  right: 12px;
  top: 50%;
  transform: translateY(-50%);
  font-size: 10px;
  color: #8b949e;
  pointer-events: none;
}
.ss-dropdown {
  position: absolute;
  top: calc(100% + 4px);
  left: 0;
  width: 100%;
  max-height: 250px;
  overflow-y: auto;
  background: #1b1b20;
  border: 1px solid #6366f1;
  border-radius: 8px;
  box-shadow: 0 10px 30px rgba(0, 0, 0, 0.5);
  z-index: 1001;
}
.ss-option {
  padding: 8px 12px;
  font-size: 13px;
  color: #c9d1d9;
  cursor: pointer;
  transition: background 0.1s;
}
.ss-option-main {
  font-size: 13px;
  line-height: 1.2;
}
.ss-option-description {
  margin-top: 4px;
  font-size: 11px;
  line-height: 1.3;
  color: #9aa4b2;
}
.ss-option:hover {
  background: rgba(99, 102, 241, 0.2);
  color: var(--text);
}
.ss-option:hover .ss-option-description,
.ss-selected .ss-option-description {
  color: #d6dbe6;
}
.ss-selected {
  background: rgba(99, 102, 241, 0.4);
  color: var(--text);
  font-weight: 600;
}
.ss-no-results {
  padding: 10px;
  font-size: 12px;
  color: #8b949e;
  text-align: center;
}
/* Scrollbar */
.ss-dropdown::-webkit-scrollbar {
  width: 6px;
}
.ss-dropdown::-webkit-scrollbar-track {
  background: transparent;
}
.ss-dropdown::-webkit-scrollbar-thumb {
  background: rgba(255, 255, 255, 0.15);
  border-radius: 3px;
}
.ss-dropdown::-webkit-scrollbar-thumb:hover {
  background: rgba(255, 255, 255, 0.25);
}
</style>
