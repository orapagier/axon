<script setup>
import { computed, onMounted, ref } from 'vue'
import { del, get, post, put } from '../lib/api.js'
import { toast } from '../lib/toast.js'
import { confirmDialog } from '../lib/confirm.js'
import { timeAgo } from '../lib/utils.js'
import Modal from '../components/Modal.vue'
import { useHeaderSearch } from '../lib/headerSearch.js'

const jobs = ref([])
const jobSearch = ref('')

useHeaderSearch('tasks', {
  query: jobSearch,
  placeholder: 'Search jobs by name, status, or platform…',
  visible: computed(() => jobs.value.length > 0),
})
const filteredJobs = computed(() => {
  const q = jobSearch.value.trim().toLowerCase()
  if (!q) return jobs.value
  return jobs.value.filter(
    (j) =>
      (j.name || '').toLowerCase().includes(q) ||
      (j.status || '').toLowerCase().includes(q) ||
      (j.platform || '').toLowerCase().includes(q)
  )
})
const jobModalOpen = ref(false)
const editingJobId = ref(null)
const jobForm = ref({
  name: '',
  task: '',
  schedule_nl: '',
  stop_condition: '',
})

const scheduleMode = ref('days')
const scheduleValue = ref(1)
const scheduleWeeklyDay = ref('MON')
const scheduleHour = ref(9)
const scheduleMinute = ref(0)
const scheduleCustom = ref('0 0 9 * * *')

const generatedCron = computed(() => {
  if (scheduleMode.value === 'minutes') return `0 */${Math.max(1, scheduleValue.value)} * * * *`
  if (scheduleMode.value === 'hours') return `0 ${scheduleMinute.value} */${Math.max(1, scheduleValue.value)} * * *`
  if (scheduleMode.value === 'days') return `0 ${scheduleMinute.value} ${scheduleHour.value} */${Math.max(1, scheduleValue.value)} * *`
  if (scheduleMode.value === 'weekly') return `0 ${scheduleMinute.value} ${scheduleHour.value} * * ${scheduleWeeklyDay.value}`
  return scheduleCustom.value
})

function showJobCreate() {
  editingJobId.value = null
  jobForm.value = { name: '', task: '', schedule_nl: '', stop_condition: '' }
  scheduleMode.value = 'days'
  scheduleValue.value = 1
  scheduleWeeklyDay.value = 'MON'
  scheduleHour.value = 9
  scheduleMinute.value = 0
  scheduleCustom.value = '0 0 9 * * *'
  jobModalOpen.value = true
}

function showJobEdit(j) {
  editingJobId.value = j.id
  jobForm.value = {
    name: j.name,
    task: j.task,
    schedule_nl: j.schedule_nl,
    stop_condition: j.stop_condition?.value || '',
  }

  const cron = j.cron_expr || j.schedule_nl || ''

  const minMatch = cron.match(/^0 \*\/(0|[1-9]\d*) \* \* \* \*$/)
  if (minMatch) {
    scheduleMode.value = 'minutes'
    scheduleValue.value = Number(minMatch[1])
    scheduleCustom.value = cron
  } else {
    const hourMatch = cron.match(/^0 ([0-5]?\d) \*\/(0|[1-9]\d*) \* \* \*$/)
    if (hourMatch) {
      scheduleMode.value = 'hours'
      scheduleMinute.value = Number(hourMatch[1])
      scheduleValue.value = Number(hourMatch[2])
      scheduleCustom.value = cron
    } else {
      const dayMatch = cron.match(/^0 ([0-5]?\d) (0|[1-9]|1\d|2[0-3]) \*\/(0|[1-9]\d*) \* \*$/)
      if (dayMatch) {
        scheduleMode.value = 'days'
        scheduleMinute.value = Math.max(0, Math.min(59, Number(dayMatch[1])))
        scheduleHour.value = Math.max(0, Math.min(23, Number(dayMatch[2])))
        scheduleValue.value = Number(dayMatch[3])
        scheduleCustom.value = cron
      } else {
        const weekMatch = cron.match(/^0 ([0-5]?\d) (0|[1-9]|1\d|2[0-3]) \* \* (SUN|MON|TUE|WED|THU|FRI|SAT)$/i)
        if (weekMatch) {
          scheduleMode.value = 'weekly'
          scheduleMinute.value = Math.max(0, Math.min(59, Number(weekMatch[1])))
          scheduleHour.value = Math.max(0, Math.min(23, Number(weekMatch[2])))
          scheduleWeeklyDay.value = weekMatch[3].toUpperCase()
          scheduleCustom.value = cron
        } else {
          scheduleMode.value = 'custom'
          scheduleValue.value = 1
          scheduleWeeklyDay.value = 'MON'
          scheduleHour.value = 9
          scheduleMinute.value = 0
          scheduleCustom.value = cron
        }
      }
    }
  }

  jobModalOpen.value = true
}

async function saveJob() {
  const cron = generatedCron.value
  if (!jobForm.value.name || !jobForm.value.task || !cron || cron.trim() === '') {
    return toast('Name, task and schedule required', false)
  }

  const body = {
    name: jobForm.value.name,
    task: jobForm.value.task,
    schedule_nl: cron,
  }
  if (jobForm.value.stop_condition) {
    body.stop_condition = {
      condition_type: 'result_contains',
      value: jobForm.value.stop_condition,
    }
  }

  const r = editingJobId.value
    ? await put(`/jobs/${editingJobId.value}`, body)
    : await post('/jobs', body)

  toast(r.ok ? (editingJobId.value ? 'Job updated' : 'Job scheduled') : r.error, r.ok)
  if (r.ok) {
    jobModalOpen.value = false
    load()
  }
}

async function runJobNow(j) {
  toast('Running job...', true)
  const r = await post(`/jobs/${j.id}/run`, {})
  toast(r.ok ? 'Job completed' : r.error, r.ok)
  load()
}

async function pauseJob(j) {
  const r = await post(`/jobs/${j.id}/pause`, '')
  toast(r.ok ? 'Paused' : r.error, r.ok)
  load()
}

async function resumeJob(j) {
  const r = await post(`/jobs/${j.id}/resume`, '')
  toast(r.ok ? 'Resumed' : r.error, r.ok)
  load()
}

async function removeJob(j) {
  const ok = await confirmDialog('This scheduled job will be permanently removed.', {
    title: 'Delete Job',
    confirmText: 'Delete',
  })
  if (!ok) return
  const r = await del(`/jobs/${j.id}/delete`)
  toast(r.ok ? 'Deleted' : r.error, r.ok)
  load()
}

function stateKey(j) {
  if (j.status === 'active') return 'ok'
  if (j.status === 'paused') return 'warn'
  return 'off'
}

const activeCount = computed(() => jobs.value.filter((j) => j.status === 'active').length)
const pausedCount = computed(() => jobs.value.filter((j) => j.status === 'paused').length)

async function load() {
  const j = await get('/jobs').catch(() => ({ jobs: [] }))
  jobs.value = j.jobs || []
}

onMounted(load)
</script>

<template>
  <div class="page-wrap tasks-page">
    <div class="page-toolbar">
      <p class="page-readout">
        <span class="readout-em">{{ jobs.length }}</span> jobs
        · <span class="readout-em">{{ activeCount }}</span> active
        <template v-if="pausedCount">
          · {{ pausedCount }} paused
        </template>
      </p>
      <div class="toolbar-actions">
        <button
          class="btn btn-ghost"
          @click="load"
        >
          Refresh
        </button>
        <button
          class="btn btn-save"
          @click="showJobCreate"
        >
          New job
        </button>
      </div>
    </div>

    <div
      v-if="filteredJobs.length === 0"
      class="empty-state"
    >
      <p class="empty-title">
        {{ jobSearch.trim() ? 'No matching jobs' : 'No scheduled jobs' }}
      </p>
      <p class="empty-hint">
        {{ jobSearch.trim() ? `Nothing matches "${jobSearch.trim()}". Try a different term.` : 'Create a job to automate recurring agent work.' }}
      </p>
    </div>

    <section
      v-else
      class="panel"
    >
      <div class="panel-head">
        <h2 class="panel-title">
          Scheduled jobs
        </h2>
        <span class="panel-count">{{ filteredJobs.length }} shown</span>
      </div>

      <div class="row-list">
        <div
          v-for="j in filteredJobs"
          :key="j.id"
          class="list-row job-row"
          :class="{ off: j.status === 'paused' }"
        >
          <div class="row-line">
            <div class="job-ident">
              <span
                class="state-dot"
                :class="stateKey(j)"
                :title="j.status"
              />
              <span class="row-title">{{ j.name }}</span>
              <span
                v-if="j.created_by === 'agent'"
                class="mono-chip"
              >agent</span>
              <span
                v-if="j.platform && j.platform !== 'dashboard'"
                class="mono-chip"
              >{{ j.platform }}</span>
              <span
                class="job-state-label"
                :class="stateKey(j)"
              >{{ j.status }}</span>
            </div>
            <div class="job-actions">
              <button
                class="btn btn-xs btn-ghost row-action"
                @click="runJobNow(j)"
              >
                Run
              </button>
              <button
                class="btn btn-xs btn-ghost row-action"
                @click="showJobEdit(j)"
              >
                Edit
              </button>
              <button
                v-if="j.status === 'active'"
                class="btn btn-xs btn-ghost row-action"
                @click="pauseJob(j)"
              >
                Pause
              </button>
              <button
                v-if="j.status === 'paused'"
                class="btn btn-xs btn-save"
                @click="resumeJob(j)"
              >
                Resume
              </button>
              <button
                class="btn btn-xs btn-danger row-action"
                @click="removeJob(j)"
              >
                Delete
              </button>
            </div>
          </div>

          <div class="job-schedule">
            <span class="mono-chip">{{ j.cron_expr }}</span>
            <span
              v-if="j.schedule_nl && j.schedule_nl !== j.cron_expr"
              class="job-schedule-nl"
            >{{ j.schedule_nl }}</span>
          </div>

          <p class="row-desc job-task">
            {{ j.task }}
          </p>

          <p class="job-readout">
            {{ j.run_count }} runs · last run {{ timeAgo(j.last_run_at) }}
          </p>
        </div>
      </div>
    </section>
  </div>

  <Modal
    v-model="jobModalOpen"
    :title="editingJobId ? `Edit Job: ${jobForm.name}` : 'Create Scheduled Job'"
  >
    <div class="job-form">
      <div class="form-field">
        <label>Job name</label>
        <input
          v-model="jobForm.name"
          type="text"
          placeholder="e.g. Daily Summary"
        >
      </div>
      <div class="form-field">
        <label>Task / instruction</label>
        <textarea
          v-model="jobForm.task"
          rows="3"
          placeholder="What should the agent do?"
        />
      </div>

      <fieldset class="schedule-builder">
        <legend>Schedule</legend>

        <div class="builder-grid">
          <div class="form-field">
            <label>Trigger mode</label>
            <select v-model="scheduleMode">
              <option value="minutes">
                Minutes
              </option>
              <option value="hours">
                Hours
              </option>
              <option value="days">
                Days
              </option>
              <option value="weekly">
                Weekly
              </option>
              <option value="custom">
                Custom (Cron)
              </option>
            </select>
          </div>

          <div
            v-if="['minutes', 'days'].includes(scheduleMode)"
            class="form-field"
          >
            <label>{{ scheduleMode.charAt(0).toUpperCase() + scheduleMode.slice(1) }} between</label>
            <input
              v-model="scheduleValue"
              type="number"
              min="1"
            >
          </div>

          <div
            v-if="scheduleMode === 'hours'"
            class="form-field"
          >
            <label>Hours between</label>
            <input
              v-model="scheduleValue"
              type="number"
              min="1"
            >
          </div>

          <div
            v-if="scheduleMode === 'hours'"
            class="form-field"
          >
            <label>At minute</label>
            <input
              v-model="scheduleMinute"
              type="number"
              min="0"
              max="59"
            >
          </div>

          <div
            v-if="scheduleMode === 'weekly'"
            class="form-field"
          >
            <label>Day</label>
            <select v-model="scheduleWeeklyDay">
              <option value="MON">
                Monday
              </option>
              <option value="TUE">
                Tuesday
              </option>
              <option value="WED">
                Wednesday
              </option>
              <option value="THU">
                Thursday
              </option>
              <option value="FRI">
                Friday
              </option>
              <option value="SAT">
                Saturday
              </option>
              <option value="SUN">
                Sunday
              </option>
            </select>
          </div>

          <div
            v-if="['days', 'weekly'].includes(scheduleMode)"
            class="form-field"
          >
            <label>At hour</label>
            <input
              v-model="scheduleHour"
              type="number"
              min="0"
              max="23"
            >
          </div>

          <div
            v-if="['days', 'weekly'].includes(scheduleMode)"
            class="form-field"
          >
            <label>At minute</label>
            <input
              v-model="scheduleMinute"
              type="number"
              min="0"
              max="59"
            >
          </div>

          <div
            v-if="scheduleMode === 'custom'"
            class="form-field span-2"
          >
            <label>Custom cron expression</label>
            <input
              v-model="scheduleCustom"
              type="text"
              placeholder="e.g. 0 0 9 * * *"
            >
          </div>
        </div>

        <p class="cron-preview">
          cron <span class="cron-preview-value">{{ generatedCron }}</span>
        </p>
      </fieldset>

      <div class="form-field">
        <label>Stop condition (result contains)</label>
        <input
          v-model="jobForm.stop_condition"
          type="text"
          placeholder="Optional: stop if output contains this text"
        >
      </div>
    </div>
    <div class="modal-actions">
      <button
        class="btn btn-ghost"
        @click="jobModalOpen = false"
      >
        Cancel
      </button>
      <button
        class="btn btn-save"
        @click="saveJob"
      >
        {{ editingJobId ? 'Save changes' : 'Schedule' }}
      </button>
    </div>
  </Modal>
</template>

<style scoped>
.tasks-page {
  padding-bottom: 60px;
}

.toolbar-actions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
}

/* ── Row identity ─────────────────────────────────────────────────────────── */
.job-ident {
  display: flex;
  align-items: center;
  gap: 9px;
  min-width: 0;
  flex-wrap: wrap;
}

.state-dot {
  width: 7px;
  height: 7px;
  border-radius: 999px;
  flex-shrink: 0;
}

.state-dot.ok { background: var(--green); }
.state-dot.warn { background: var(--yellow); }
.state-dot.off { background: var(--muted); opacity: 0.5; }

.job-state-label {
  font-family: var(--font-mono);
  font-size: 0.62rem;
  letter-spacing: 0.04em;
  text-transform: uppercase;
  color: var(--muted);
}

.job-state-label.warn { color: var(--yellow); }

/* ── Row actions: quiet until the row is engaged ──────────────────────────── */
.job-actions {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-shrink: 0;
}

.row-action {
  opacity: 0.25;
  transition: opacity 0.15s ease;
}

.job-row:hover .row-action,
.row-action:focus-visible {
  opacity: 1;
}

@media (hover: none) {
  .row-action {
    opacity: 1;
  }
}

/* ── Schedule + task readouts ─────────────────────────────────────────────── */
.job-schedule {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 8px;
  margin-top: 7px;
}

.job-schedule-nl {
  font-family: var(--font-mono);
  font-size: 0.68rem;
  color: var(--muted);
}

.job-task {
  font-family: var(--font-mono);
  font-size: 0.72rem;
  margin-top: 8px;
  overflow-wrap: anywhere;
}

.job-readout {
  margin: 8px 0 0;
  font-family: var(--font-mono);
  font-size: 0.64rem;
  color: var(--muted);
}

/* A paused job fades back into the membrane. */
.job-row.off .row-title {
  color: var(--muted);
}

.job-row.off .job-schedule,
.job-row.off .job-task,
.job-row.off .job-readout {
  opacity: 0.55;
}

/* ── Modal form ───────────────────────────────────────────────────────────── */
.job-form {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.form-field label {
  margin-top: 0;
}

.form-field input,
.form-field select,
.form-field textarea {
  margin-bottom: 0;
}

.schedule-builder {
  margin: 0;
  padding: 12px 14px 14px;
  border: 1px solid var(--border);
  border-radius: var(--r-md);
  background: color-mix(in srgb, var(--text) 2%, transparent);
}

.schedule-builder legend {
  padding: 0 6px;
  font-family: var(--font-display);
  font-size: 0.66rem;
  font-weight: 600;
  letter-spacing: 0.14em;
  text-transform: uppercase;
  color: var(--muted);
}

.builder-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 10px;
}

.span-2 {
  grid-column: span 2;
}

.cron-preview {
  margin: 12px 0 0;
  font-family: var(--font-mono);
  font-size: 0.68rem;
  color: var(--muted);
}

.cron-preview-value {
  color: var(--accent);
  font-weight: 600;
}

.modal-actions {
  display: flex;
  justify-content: flex-end;
  gap: 10px;
  margin-top: 18px;
  padding-top: 14px;
  border-top: 1px solid var(--border);
}

@media (max-width: 700px) {
  .builder-grid {
    grid-template-columns: 1fr;
  }

  .span-2 {
    grid-column: auto;
  }

  .job-actions {
    flex-wrap: wrap;
  }
}
</style>
