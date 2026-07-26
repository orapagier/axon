<script setup>
import { ref } from 'vue'
import Modal from './Modal.vue'
import { postForm } from '../lib/api.js'
import { createTakeRecorder } from '../lib/wakeEnrollment.js'

defineProps({
  modelValue: { type: Boolean, default: false },
})
const emit = defineEmits(['update:modelValue', 'enrolled'])

const TAKE_COUNT = 5
const MIN_TAKES = 3

const phrase = ref('Hey Axon')
const takes = ref([]) // Blob[], one WAV per take
const recording = ref(false)
const level = ref(0)
const building = ref(false)
const built = ref(false)
const status = ref('')

let recorder = null

function stopRecorderIfAny() {
  if (recorder) {
    recorder.stop()
    recorder = null
  }
  recording.value = false
  level.value = 0
}

function onModalChange(open) {
  if (!open) {
    stopRecorderIfAny()
    emit('update:modelValue', false)
  }
}

async function startTake() {
  if (recording.value || built.value || takes.value.length >= TAKE_COUNT) return
  status.value = ''
  try {
    recorder = createTakeRecorder()
    await recorder.start((rms) => {
      level.value = rms
    })
    recording.value = true
  } catch (e) {
    status.value = e?.message || 'Microphone unavailable'
  }
}

function stopTake() {
  if (!recording.value || !recorder) return
  const wav = recorder.stop()
  recorder = null
  recording.value = false
  level.value = 0
  takes.value.push(wav)
}

function redoLastTake() {
  if (takes.value.length) takes.value.pop()
}

async function buildModel() {
  building.value = true
  status.value = 'Building…'
  const form = new FormData()
  form.append('name', phrase.value.trim() || 'Hey Axon')
  takes.value.forEach((wav, i) => form.append('sample', wav, `take${i}.wav`))
  try {
    const res = await postForm('/wakeword/build', form)
    if (res?.ok) {
      built.value = true
      status.value = `Done — “${res.phrase}” is ready.`
      emit('enrolled', res.phrase)
    } else {
      status.value = res?.error || "Couldn't build a model — try re-recording more clearly."
    }
  } catch (e) {
    status.value = e?.message || "Couldn't build a model — try re-recording more clearly."
  } finally {
    building.value = false
  }
}

function done() {
  emit('update:modelValue', false)
}
</script>

<template>
  <Modal
    :model-value="modelValue"
    title="Set up your wake word"
    max-width="440px"
    @update:model-value="onModalChange"
  >
    <p class="enroll-intro">
      Choose a phrase and record yourself saying it a few times — this builds a wake-word model tuned
      to your voice.
    </p>

    <input
      v-model="phrase"
      type="text"
      class="premium-input"
      placeholder="Wake phrase"
      :disabled="recording || built"
    >

    <div class="enroll-level">
      <div
        class="enroll-level-fill"
        :style="{ width: Math.min(100, level * 400) + '%' }"
      />
    </div>

    <p class="enroll-status">
      {{ status || `${takes.length} of ${TAKE_COUNT} takes recorded` }}
    </p>

    <div class="modal-actions">
      <button
        class="btn btn-ghost"
        type="button"
        :disabled="recording || built || !takes.length"
        @click="redoLastTake"
      >
        Re-record last take
      </button>
      <button
        v-if="!recording"
        class="btn btn-primary"
        type="button"
        :disabled="built || takes.length >= TAKE_COUNT"
        @click="startTake"
      >
        {{ takes.length ? 'Record next take' : 'Record' }}
      </button>
      <button
        v-else
        class="btn btn-primary"
        type="button"
        @click="stopTake"
      >
        Stop
      </button>
    </div>

    <button
      v-if="!built"
      class="btn btn-primary enroll-build-btn"
      type="button"
      :disabled="recording || building || takes.length < MIN_TAKES"
      @click="buildModel"
    >
      Build wake word
    </button>
    <button
      v-else
      class="btn btn-primary enroll-build-btn"
      type="button"
      @click="done"
    >
      Done
    </button>
  </Modal>
</template>

<style scoped>
.enroll-intro {
  color: var(--muted);
  font-size: 13px;
  line-height: 1.55;
  margin: 0 0 12px;
}
.enroll-level {
  height: 6px;
  border-radius: 3px;
  background: var(--border);
  margin-top: 14px;
  overflow: hidden;
}
.enroll-level-fill {
  height: 100%;
  background: var(--accent);
  transition: width 0.08s linear;
}
.enroll-status {
  color: var(--muted);
  font-size: 13px;
  margin: 10px 0 0;
}
.enroll-build-btn {
  width: 100%;
  margin-top: 16px;
}
</style>
