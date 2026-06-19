<script setup>
import { ref } from 'vue'
import { toast } from '../lib/toast.js'

const masterKey = ref('')
const loading = ref(false)

const emit = defineEmits(['login'])

async function handleLogin() {
  if (!masterKey.value) {
    return toast('Master Key is required', false)
  }

  loading.value = true

  try {
    // Verify key against the API before storing.
    const r = await fetch('/api/settings', {
      headers: { Authorization: `Bearer ${masterKey.value}` },
    })

    if (r.status === 401) {
      toast('Invalid Master Key', false)
      loading.value = false
      return
    }

    localStorage.setItem('AXON_MASTER_KEY', masterKey.value)
    emit('login')
  } catch (e) {
    toast('Connection error', false)
  } finally {
    loading.value = false
  }
}
</script>

<template>
  <div class="login-page">
    <div class="login-card premium-card">
      <div class="login-header">
        <img src="/favicon.png" alt="Axon" class="login-logo" />
        <h1>AXON</h1>
        <p>Secure access to your agent workspace</p>
      </div>

      <form @submit.prevent="handleLogin" class="login-form">
        <div class="form-group-modern">
          <label>Master Access Key</label>
          <input
            type="password"
            v-model="masterKey"
            class="premium-input"
            placeholder="Paste your master key"
            required
            autofocus
          />
        </div>

        <button type="submit" class="btn btn-save login-btn" :disabled="loading">
          {{ loading ? 'Authenticating...' : 'Enter Dashboard' }}
        </button>
      </form>

      <div class="login-footer">
        <p>Refer to your <code>.env</code> file for <code>AXON_MASTER_KEY</code></p>
      </div>
    </div>
  </div>
</template>

<style scoped>
.login-page {
  display: flex;
  align-items: center;
  justify-content: center;
  height: 100vh;
  width: 100vw;
  background: radial-gradient(circle at center, #ffffff 0%, #dde2ee 100%);
}

.login-card {
  width: 100%;
  max-width: 400px;
  padding: 40px;
  text-align: center;
  animation: fadeIn 0.5s ease-out;
}

@keyframes fadeIn {
  from {
    opacity: 0;
    transform: translateY(20px);
  }

  to {
    opacity: 1;
    transform: translateY(0);
  }
}

.login-logo {
  width: 64px;
  height: 64px;
  margin-bottom: 16px;
}

.login-header h1 {
  font-size: 28px;
  letter-spacing: 0.1em;
  margin-bottom: 4px;
  color: var(--text);
}

.login-header p {
  color: var(--muted);
  font-size: 14px;
  margin-bottom: 32px;
}

.login-form {
  display: flex;
  flex-direction: column;
  gap: 20px;
  text-align: left;
}

.login-btn {
  width: 100%;
  padding: 12px;
  font-size: 15px;
  margin-top: 10px;
}

.login-footer {
  margin-top: 32px;
  font-size: 12px;
  color: rgba(0, 0, 0, 0.3);
}

.login-footer code {
  background: rgba(0, 0, 0, 0.05);
  padding: 2px 6px;
  border-radius: 4px;
  color: #a29bfe;
}
</style>
