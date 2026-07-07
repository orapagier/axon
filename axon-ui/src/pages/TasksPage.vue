<script setup>
import { computed, onMounted, ref } from 'vue'
import { del, get, post, put } from '../lib/api.js'
import { toast } from '../lib/toast.js'
import { confirmDialog } from '../lib/confirm.js'
import { timeAgo } from '../lib/utils.js'
import Modal from '../components/Modal.vue'
import Pill from '../components/Pill.vue'

const jobs = ref([])
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

function jobStatusType(s) {
  if (s === 'active') return 'ok'
  if (s === 'paused') return 'warn'
  return 'muted'
}

async function load() {
  const j = await get('/jobs').catch(() => ({ jobs: [] }))
  jobs.value = j.jobs || []
}

onMounted(load)
</script>

<template>
  <div class="services-page tasks-page">
    <div class="page-header-container">
      <div class="page-header">
        <h1>Tasks</h1>
        <p class="page-desc">Manage automated cron jobs in one place.</p>
      </div>
      <div class="header-actions">
        <button class="btn btn-save" @click="showJobCreate">New Job</button>
        <button class="btn btn-ghost" @click="load">Refresh</button>
      </div>
    </div>

    <div class="tasks-grid">
      <section class="premium-card">
        <div class="card-header-row no-collapse">
          <div class="card-title-group">
            <h2>Scheduled Jobs</h2>
          </div>
          <span class="card-summary">{{ jobs.length }} active</span>
        </div>

        <div v-if="jobs.length === 0" class="empty-state">No scheduled jobs found. Create one to automate tasks.</div>
        <div v-else class="service-list">
          <div v-for="j in jobs" :key="j.id" class="service-item job-row" :class="{ disabled: j.status === 'paused' }">
            <div class="service-info">
              <div class="service-name-row">
                <div class="service-name-group">
                  <span class="service-name">{{ j.name }}</span>
                  <Pill v-if="j.created_by === 'agent'" type="info" text="AGENT-CREATED" />
                  <Pill :type="jobStatusType(j.status)" :text="j.status.toUpperCase()" />
                  <Pill v-if="j.platform !== 'dashboard'" type="info" :text="j.platform.toUpperCase()" />
                </div>
                <div class="service-actions">
                  <button class="btn btn-sm btn-primary" @click="runJobNow(j)">Run</button>
                  <button class="btn btn-sm btn-ghost" @click="showJobEdit(j)">Edit</button>
                  <button v-if="j.status === 'active'" class="btn btn-sm btn-ghost" @click="pauseJob(j)">Pause</button>
                  <button v-if="j.status === 'paused'" class="btn btn-sm btn-save" @click="resumeJob(j)">Resume</button>
                  <button class="btn btn-sm btn-danger" @click="removeJob(j)">Delete</button>
                </div>
              </div>

              <div class="job-meta-row">
                <span class="cron-text">{{ j.cron_expr }}</span>
                <span class="divider">|</span>
                <span class="nl-text">{{ j.schedule_nl }}</span>
              </div>

              <div class="job-task-preview">
                <div class="preview-label">Task Instruction</div>
                <p class="task-text">{{ j.task }}</p>
              </div>

              <div class="job-stats-meta">
                <span class="stat">Runs: <strong>{{ j.run_count }}</strong></span>
                <span class="divider">|</span>
                <span class="stat">Last Run: <strong>{{ timeAgo(j.last_run_at) }}</strong></span>
              </div>
            </div>
          </div>
        </div>
      </section>
    </div>
  </div>

  <Modal v-model="jobModalOpen" :title="editingJobId ? `Edit Job: ${jobForm.name}` : 'Create Scheduled Job'">
    <div class="form-container">
      <div class="form-group-modern">
        <label>Job Name</label>
        <input type="text" v-model="jobForm.name" class="premium-input" placeholder="e.g. Daily Summary" />
      </div>
      <div class="form-group-modern">
        <label>Task / Instruction</label>
        <textarea v-model="jobForm.task" rows="3" class="premium-input textarea-input" placeholder="What should the agent do?"></textarea>
      </div>

      <div class="schedule-builder">
        <label class="builder-title">Schedule Configuration</label>

        <div class="builder-grid">
          <div class="form-group-modern">
            <label>Trigger Mode</label>
            <select v-model="scheduleMode" class="premium-input select-input">
              <option value="minutes">Minutes</option>
              <option value="hours">Hours</option>
              <option value="days">Days</option>
              <option value="weekly">Weekly</option>
              <option value="custom">Custom (Cron)</option>
            </select>
          </div>

          <div v-if="['minutes', 'days'].includes(scheduleMode)" class="form-group-modern">
            <label>{{ scheduleMode.charAt(0).toUpperCase() + scheduleMode.slice(1) }} Between</label>
            <input type="number" v-model="scheduleValue" class="premium-input" min="1" />
          </div>

          <div v-if="scheduleMode === 'hours'" class="form-group-modern">
            <label>Hours Between</label>
            <input type="number" v-model="scheduleValue" class="premium-input" min="1" />
          </div>

          <div v-if="scheduleMode === 'hours'" class="form-group-modern">
            <label>At Minute</label>
            <input type="number" v-model="scheduleMinute" class="premium-input" min="0" max="59" />
          </div>

          <div v-if="scheduleMode === 'weekly'" class="form-group-modern">
            <label>Day</label>
            <select v-model="scheduleWeeklyDay" class="premium-input select-input">
              <option value="MON">Monday</option>
              <option value="TUE">Tuesday</option>
              <option value="WED">Wednesday</option>
              <option value="THU">Thursday</option>
              <option value="FRI">Friday</option>
              <option value="SAT">Saturday</option>
              <option value="SUN">Sunday</option>
            </select>
          </div>

          <div v-if="['days', 'weekly'].includes(scheduleMode)" class="form-group-modern">
            <label>At Hour</label>
            <input type="number" v-model="scheduleHour" class="premium-input" min="0" max="23" />
          </div>

          <div v-if="['days', 'weekly'].includes(scheduleMode)" class="form-group-modern">
            <label>At Minute</label>
            <input type="number" v-model="scheduleMinute" class="premium-input" min="0" max="59" />
          </div>

          <div v-if="scheduleMode === 'custom'" class="form-group-modern span-2">
            <label>Custom Cron Expression</label>
            <input type="text" v-model="scheduleCustom" class="premium-input" placeholder="e.g. 0 0 9 * * *" />
          </div>
        </div>
      </div>

      <div class="form-group-modern">
        <label>Generated Cron Preview</label>
        <input type="text" :value="generatedCron" disabled class="premium-input mono muted-input" />
      </div>
      <div class="form-group-modern">
        <label>Stop Condition (Result contains)</label>
        <input type="text" v-model="jobForm.stop_condition" class="premium-input" placeholder="Optional: stop if output contains this text" />
      </div>
    </div>
    <div class="modal-actions-modern">
      <button class="btn btn-ghost" @click="jobModalOpen = false">Cancel</button>
      <button class="btn btn-save" @click="saveJob">{{ editingJobId ? 'Save Changes' : 'Schedule' }}</button>
    </div>
  </Modal>
</template>

<style scoped>
.tasks-page {
  padding-bottom: 60px;
}

.tasks-grid {
  display: grid;
  grid-template-columns: 1fr;
  gap: 16px;
  align-items: start;
}

.empty-state {
  padding: 40px;
  text-align: center;
  color: var(--muted);
  font-size: 14px;
}

.service-item.disabled {
  opacity: 0.6;
}

.service-name-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
}

.service-name-group {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
}

.service-actions {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
}

.service-name {
  font-size: 16px;
  font-weight: 700;
}

.job-meta-row {
  margin-top: 12px;
  display: flex;
  align-items: center;
  gap: 8px;
}

.cron-text {
  font-family: monospace;
  color: var(--teal);
  font-size: 13px;
  font-weight: 600;
}

.nl-text {
  color: var(--green);
  font-size: 13px;
}

.divider {
  color: var(--muted);
}

.job-task-preview {
  margin-top: 14px;
  background: rgba(0, 0, 0, 0.2);
  padding: 12px;
  border-radius: 8px;
}

.preview-label {
  font-size: 11px;
  font-weight: 800;
  text-transform: uppercase;
  margin-bottom: 6px;
  color: var(--muted);
}

.task-text {
  margin: 0;
  font-size: 13px;
  font-family: monospace;
  line-height: 1.5;
}

.job-stats-meta {
  margin-top: 14px;
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
  color: var(--muted);
}

.form-container {
  display: flex;
  flex-direction: column;
  gap: 12px;
  margin-bottom: 20px;
}

.form-group-modern {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.form-group-modern label {
  font-size: 12px;
  font-weight: 700;
  text-transform: uppercase;
  color: var(--muted);
  letter-spacing: 0.05em;
}

.schedule-builder {
  border: 1px solid rgba(0, 0, 0, 0.08);
  border-radius: 10px;
  padding: 14px;
  background: rgba(0, 0, 0, 0.18);
}

.builder-title {
  display: block;
  margin-bottom: 12px;
  font-size: 11px;
  font-weight: 800;
  color: var(--muted);
  text-transform: uppercase;
}

.builder-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 10px;
}

.span-2 {
  grid-column: span 2;
}

.mono {
  font-family: monospace;
}

.muted-input {
  opacity: 0.75;
}

.modal-actions-modern {
  display: flex;
  justify-content: flex-end;
  gap: 12px;
  padding-top: 16px;
  border-top: 1px solid rgba(0, 0, 0, 0.06);
}

@media (max-width: 960px) {
  .service-name-row {
    flex-direction: column;
    align-items: flex-start;
  }

  .builder-grid {
    grid-template-columns: 1fr;
  }

  .span-2 {
    grid-column: auto;
  }
}
</style>
