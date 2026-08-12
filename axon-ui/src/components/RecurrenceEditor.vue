<script setup>
/*
 * Guided editor for a Google Calendar `recurrence` value.
 *
 * The field is an array of RFC 5545 rule strings — "RRULE:FREQ=WEEKLY;BYDAY=MO"
 * — which is not something a non-technical user can be asked to write. This
 * turns the common cases (every N days/weeks/months/years, on chosen weekdays,
 * ending never / after N times / on a date) into plain controls and assembles
 * the rule itself.
 *
 * Anything the controls cannot express — EXDATE, RDATE, BYSETPOS, a rule pasted
 * from elsewhere — falls through to a raw text box rather than being silently
 * rewritten, so an existing workflow never loses a pattern by being opened.
 */
import { ref, computed, watch } from 'vue'

const props = defineProps({
  // Array of rule strings, but tolerates the bare string older nodes stored.
  modelValue: { type: [Array, String], default: () => [] },
})
const emit = defineEmits(['update:modelValue'])

const WEEKDAYS = [
  { key: 'MO', label: 'Mon' },
  { key: 'TU', label: 'Tue' },
  { key: 'WE', label: 'Wed' },
  { key: 'TH', label: 'Thu' },
  { key: 'FR', label: 'Fri' },
  { key: 'SA', label: 'Sat' },
  { key: 'SU', label: 'Sun' },
]

const FREQUENCIES = [
  { value: 'DAILY', unit: 'day', label: 'Day' },
  { value: 'WEEKLY', unit: 'week', label: 'Week' },
  { value: 'MONTHLY', unit: 'month', label: 'Month' },
  { value: 'YEARLY', unit: 'year', label: 'Year' },
]

const mode = ref('none') // none | guided | custom
const freq = ref('WEEKLY')
const interval = ref(1)
const byDay = ref([])
const endMode = ref('never') // never | count | until
const count = ref(10)
const until = ref('')
const customText = ref('')

/** Rules as an array of trimmed, non-empty strings, whatever shape came in. */
function asRules(value) {
  if (Array.isArray(value)) return value.map((r) => String(r).trim()).filter(Boolean)
  return String(value || '')
    .split('\n')
    .map((r) => r.trim())
    .filter(Boolean)
}

/** "20261231T000000Z" or "20261231" → "2026-12-31" for the date input. */
function untilToDate(raw) {
  const m = String(raw).match(/^(\d{4})(\d{2})(\d{2})/)
  return m ? `${m[1]}-${m[2]}-${m[3]}` : ''
}

/**
 * Load an existing value into the controls. Returns false when the value is
 * something the guided controls would not round-trip faithfully, which puts the
 * editor into custom mode instead of quietly dropping the parts it can't show.
 */
function loadGuided(rules) {
  if (rules.length !== 1) return false
  const rule = rules[0]
  if (!/^RRULE:/i.test(rule)) return false

  const parts = {}
  for (const chunk of rule.slice(6).split(';')) {
    const [k, v] = chunk.split('=')
    if (k) parts[k.toUpperCase()] = (v || '').toUpperCase()
  }

  const known = ['FREQ', 'INTERVAL', 'BYDAY', 'COUNT', 'UNTIL', 'WKST']
  if (Object.keys(parts).some((k) => !known.includes(k))) return false
  if (!FREQUENCIES.some((f) => f.value === parts.FREQ)) return false
  // A positional BYDAY ("2TU" = second Tuesday) isn't expressible here.
  if (parts.BYDAY && !parts.BYDAY.split(',').every((d) => WEEKDAYS.some((w) => w.key === d))) {
    return false
  }

  freq.value = parts.FREQ
  interval.value = Math.max(1, parseInt(parts.INTERVAL || '1', 10) || 1)
  byDay.value = parts.BYDAY ? parts.BYDAY.split(',') : []
  if (parts.COUNT) {
    endMode.value = 'count'
    count.value = Math.max(1, parseInt(parts.COUNT, 10) || 1)
  } else if (parts.UNTIL) {
    endMode.value = 'until'
    until.value = untilToDate(parts.UNTIL)
  } else {
    endMode.value = 'never'
  }
  return true
}

// Seed the controls from whatever the node already holds. Only re-seeds when
// the value changes from the outside (node switch, undo) — `emitRules` marks
// its own writes so typing in the editor doesn't reset the controls mid-edit.
let ownWrite = null
watch(
  () => props.modelValue,
  (value) => {
    const rules = asRules(value)
    if (ownWrite !== null && JSON.stringify(rules) === JSON.stringify(ownWrite)) return
    ownWrite = null
    if (rules.length === 0) {
      mode.value = 'none'
      return
    }
    if (loadGuided(rules)) {
      mode.value = 'guided'
    } else {
      mode.value = 'custom'
      customText.value = rules.join('\n')
    }
  },
  { immediate: true }
)

/** Assemble the RRULE the controls currently describe. */
const guidedRule = computed(() => {
  const bits = [`FREQ=${freq.value}`]
  const n = Math.max(1, parseInt(interval.value, 10) || 1)
  if (n > 1) bits.push(`INTERVAL=${n}`)
  if (freq.value === 'WEEKLY' && byDay.value.length > 0) {
    // Keep calendar order regardless of the order the boxes were ticked.
    const ordered = WEEKDAYS.filter((w) => byDay.value.includes(w.key)).map((w) => w.key)
    bits.push(`BYDAY=${ordered.join(',')}`)
  }
  if (endMode.value === 'count') {
    bits.push(`COUNT=${Math.max(1, parseInt(count.value, 10) || 1)}`)
  } else if (endMode.value === 'until' && until.value) {
    // UNTIL must be a UTC instant; end-of-day keeps the last occurrence in.
    bits.push(`UNTIL=${until.value.replace(/-/g, '')}T235959Z`)
  }
  return `RRULE:${bits.join(';')}`
})

function emitRules(rules) {
  ownWrite = rules
  emit('update:modelValue', rules)
}

function push() {
  if (mode.value === 'none') return emitRules([])
  if (mode.value === 'custom') return emitRules(asRules(customText.value))
  return emitRules([guidedRule.value])
}

watch([mode, freq, interval, byDay, endMode, count, until, customText], push, { deep: true })

function toggleDay(key) {
  byDay.value = byDay.value.includes(key)
    ? byDay.value.filter((d) => d !== key)
    : [...byDay.value, key]
}

const unit = computed(() => {
  const f = FREQUENCIES.find((x) => x.value === freq.value)
  const n = Math.max(1, parseInt(interval.value, 10) || 1)
  return f ? (n === 1 ? f.unit : `${f.unit}s`) : ''
})

/** Plain-English echo of the rule, so the user can check it without reading RRULE. */
const summary = computed(() => {
  if (mode.value === 'none') return 'Happens once — does not repeat.'
  if (mode.value === 'custom') {
    return customText.value.trim() ? 'Using the rule written below.' : 'No rule entered yet.'
  }
  const n = Math.max(1, parseInt(interval.value, 10) || 1)
  let text = n === 1 ? `Every ${unit.value}` : `Every ${n} ${unit.value}`
  if (freq.value === 'WEEKLY' && byDay.value.length > 0) {
    const names = WEEKDAYS.filter((w) => byDay.value.includes(w.key)).map((w) => w.label)
    text += ` on ${names.join(', ')}`
  }
  if (endMode.value === 'count') text += `, ${count.value} times`
  else if (endMode.value === 'until' && until.value) text += `, until ${until.value}`
  else text += ', forever'
  return `${text}.`
})
</script>

<template>
  <div class="rec-editor">
    <div class="rec-row">
      <select
        v-model="mode"
        class="rec-select"
      >
        <option value="none">
          Does not repeat
        </option>
        <option value="guided">
          Repeats…
        </option>
        <option value="custom">
          Advanced (write the rule myself)
        </option>
      </select>
    </div>

    <template v-if="mode === 'guided'">
      <div class="rec-row">
        <span class="rec-label">Every</span>
        <input
          v-model="interval"
          type="number"
          min="1"
          max="999"
          class="rec-number"
        >
        <select
          v-model="freq"
          class="rec-select rec-grow"
        >
          <option
            v-for="f in FREQUENCIES"
            :key="f.value"
            :value="f.value"
          >
            {{ f.label }}
          </option>
        </select>
      </div>

      <div
        v-if="freq === 'WEEKLY'"
        class="rec-row rec-days"
      >
        <button
          v-for="d in WEEKDAYS"
          :key="d.key"
          type="button"
          class="rec-day"
          :class="{ 'rec-day-on': byDay.includes(d.key) }"
          @click="toggleDay(d.key)"
        >
          {{ d.label }}
        </button>
      </div>

      <div class="rec-row">
        <span class="rec-label">Ends</span>
        <select
          v-model="endMode"
          class="rec-select rec-grow"
        >
          <option value="never">
            Never
          </option>
          <option value="count">
            After a number of times
          </option>
          <option value="until">
            On a date
          </option>
        </select>
      </div>

      <div
        v-if="endMode === 'count'"
        class="rec-row"
      >
        <span class="rec-label">After</span>
        <input
          v-model="count"
          type="number"
          min="1"
          max="999"
          class="rec-number"
        >
        <span class="rec-label">times</span>
      </div>

      <div
        v-if="endMode === 'until'"
        class="rec-row"
      >
        <span class="rec-label">On</span>
        <input
          v-model="until"
          type="date"
          class="rec-select rec-grow"
        >
      </div>
    </template>

    <div
      v-if="mode === 'custom'"
      class="rec-row"
    >
      <textarea
        v-model="customText"
        rows="3"
        class="rec-textarea"
        placeholder="RRULE:FREQ=MONTHLY;BYDAY=2TU&#10;One rule per line"
      />
    </div>

    <div class="rec-summary">
      {{ summary }}
    </div>
  </div>
</template>

<style scoped>
.rec-editor {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.rec-row {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}
.rec-label {
  font-size: 12px;
  color: #9aa4b2;
  white-space: nowrap;
}
.rec-select,
.rec-number,
.rec-textarea {
  background: rgba(255, 255, 255, 0.04);
  border: 1px solid rgba(255, 255, 255, 0.1);
  color: #f2f7ff;
  padding: 8px 10px;
  border-radius: 6px;
  font-size: 13px;
  outline: none;
  box-sizing: border-box;
  font-family: inherit;
}
.rec-select:focus,
.rec-number:focus,
.rec-textarea:focus {
  border-color: #6366f1;
  box-shadow: 0 0 0 3px rgba(99, 102, 241, 0.15);
}
.rec-select {
  min-width: 0;
}
.rec-grow {
  flex: 1;
}
.rec-number {
  width: 74px;
}
.rec-textarea {
  width: 100%;
  resize: vertical;
}
.rec-days {
  gap: 4px;
}
.rec-day {
  flex: 1;
  min-width: 38px;
  padding: 6px 0;
  border-radius: 6px;
  border: 1px solid rgba(255, 255, 255, 0.1);
  background: rgba(255, 255, 255, 0.04);
  color: #9aa4b2;
  font-size: 11px;
  cursor: pointer;
  transition: all 0.12s;
}
.rec-day:hover {
  border-color: rgba(99, 102, 241, 0.6);
  color: #f2f7ff;
}
.rec-day-on {
  background: rgba(99, 102, 241, 0.35);
  border-color: #6366f1;
  color: #f2f7ff;
  font-weight: 600;
}
.rec-summary {
  font-size: 11px;
  line-height: 1.4;
  color: #9aa4b2;
  font-style: italic;
}
</style>
