<script setup>
// Expression text input used by every "picker" field (dropdowns, multi-selects,
// dates, booleans) once it is switched into expression mode. Self-contained:
// owns its focus/preview state and styles so it renders identically wherever a
// field type lives (top-level or nested in a collection). The parent supplies a
// `resolve` callback so all expression resolution stays in one place.
import { ref, computed } from 'vue'

const props = defineProps({
  modelValue: { type: [String, Number, Boolean, Object, Array], default: '' },
  // (raw) => resolved string | null  (null = nothing resolved yet)
  resolve: { type: Function, default: null },
  placeholder: { type: String, default: 'Drop a field or type an expression…' },
})
const emit = defineEmits(['update:modelValue', 'revert'])

const focused = ref(false)
const inputEl = ref(null)

function hasExpression(val) {
  if (typeof val !== 'string') return false
  if (val.includes('{{')) return true
  return /\$node(\.|\[['"])[A-Za-z0-9 _-]/.test(val)
}

const valueStr = computed(() => (props.modelValue == null ? '' : String(props.modelValue)))
const isExpr = computed(() => hasExpression(valueStr.value))
const resolved = computed(() => (props.resolve ? props.resolve(valueStr.value) : null))

function onInput(e) {
  emit('update:modelValue', e.target.value)
}

// Cursor-aware insertion so a dropped token lands where the caret is, matching
// the behaviour of the plain text fields in NodeDetails.
function onDrop(e) {
  const token = e.dataTransfer.getData('variable')
  if (!token) return
  const el = e.target
  const current = valueStr.value
  if (el && typeof el.selectionStart === 'number') {
    const pos = el.selectionStart
    emit('update:modelValue', current.substring(0, pos) + token + current.substring(pos))
  } else {
    emit('update:modelValue', current + token)
  }
}
</script>

<template>
  <div class="expr-input-wrap">
    <input
      ref="inputEl"
      type="text"
      :value="valueStr"
      :class="{ 'has-expression': isExpr, 'focused-exp': focused && isExpr }"
      :placeholder="placeholder"
      @input="onInput"
      @drop.prevent="onDrop"
      @dragover.prevent
      @focus="focused = true"
      @blur="focused = false"
    >
    <button
      type="button"
      class="btn-fx active"
      title="Use a fixed value"
      @click="emit('revert')"
    >
      ƒx
    </button>

    <!-- Focused RESULT popover -->
    <Transition name="fade">
      <div
        v-if="focused && isExpr"
        class="nd-dropdown-preview"
      >
        <div class="fp-header">
          <span>RESULT</span>
        </div>
        <div class="fp-body">
          {{ resolved ?? '(Waiting for data…)' }}
        </div>
      </div>
    </Transition>

    <!-- Persistent resolved value (shown when not focused) -->
    <div
      v-if="isExpr && !focused"
      class="exp-resolved"
    >
      <span class="exp-resolved-icon">=</span>
      <span class="exp-resolved-val">{{ resolved ?? '(run previous node to preview)' }}</span>
    </div>
  </div>
</template>

<style scoped>
.expr-input-wrap { position: relative; width: 100%; }
.expr-input-wrap input {
  width: 100%;
  background: rgba(255, 255, 255, 0.04);
  border: 1px solid rgba(255, 255, 255, 0.1);
  color: #f2f7ff;
  padding: 6px 32px 6px 10px;
  border-radius: 6px;
  font-family: inherit;
  font-size: 13px;
  outline: none;
  transition: all 0.2s;
}
.expr-input-wrap input:focus { background: rgba(255, 255, 255, 0.06); border-color: #6366f1; }

.has-expression {
  background: rgba(99,102,241,0.06) !important;
  border-color: rgba(99,102,241,0.4) !important;
  color: #a5b4fc !important;
  font-family: 'Fira Code', 'JetBrains Mono', monospace !important;
}
.focused-exp {
  border-color: #6366f1 !important;
  border-bottom-left-radius: 0 !important;
  border-bottom-right-radius: 0 !important;
}

/* ƒx toggle — small pill at the right edge of the field */
.btn-fx {
  position: absolute;
  top: 50%;
  right: 4px;
  transform: translateY(-50%);
  height: 20px;
  min-width: 22px;
  padding: 0 5px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  background: rgba(27, 27, 32, 0.9);
  border: 1px solid rgba(255, 255, 255, 0.12);
  border-radius: 5px;
  color: #a6a6b2;
  font-family: 'Fira Code', monospace;
  font-size: 12px;
  font-style: italic;
  cursor: pointer;
  z-index: 6;
  transition: color 0.15s, border-color 0.15s, background 0.15s;
}
.btn-fx:hover { color: #a5b4fc; border-color: rgba(99,102,241,0.5); background: rgba(40,40,55,0.95); }
.btn-fx.active { color: #818cf8; border-color: rgba(99,102,241,0.5); background: rgba(99,102,241,0.12); }

.nd-dropdown-preview {
  position: absolute; top: 100%; left: 0; width: 100%;
  background: #1b1b20;
  border: 1px solid #6366f1;
  border-top: none;
  border-radius: 0 0 8px 8px;
  box-shadow: 0 10px 30px rgba(0,0,0,0.5);
  z-index: 1000; overflow: hidden;
}
.fp-header { padding: 4px 10px; background: rgba(99,102,241,0.12); border-bottom: 1px solid rgba(99,102,241,0.2); }
.fp-header span { font-size: 9px; font-weight: 800; color: #818cf8; letter-spacing: 0.12em; }
.fp-body {
  padding: 8px 12px;
  font-family: 'Fira Code', monospace;
  font-size: 11px; color: #f2f7ff;
  background: #15151a; max-height: 160px; overflow-y: auto;
  white-space: pre-wrap; word-break: break-all;
}

.fade-enter-active, .fade-leave-active { transition: opacity 0.18s; }
.fade-enter-from, .fade-leave-to { opacity: 0; }

.exp-resolved {
  display: flex; align-items: center; gap: 6px;
  margin-top: 4px; padding: 1px 2px; max-width: 100%;
  font-family: 'Fira Code', 'JetBrains Mono', monospace;
  font-size: 10.5px; line-height: 1.4; color: #7dd3a8; overflow: hidden;
}
.exp-resolved-icon { color: #818cf8; font-weight: 800; flex: 0 0 auto; }
.exp-resolved-val { overflow: hidden; white-space: nowrap; text-overflow: ellipsis; opacity: 0.9; }
</style>
