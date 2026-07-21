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
    <div class="login-card">
      <div class="login-header">
        <img
          src="/favicon.png"
          alt="Axon"
          class="login-logo"
        >
        <h1>AXON</h1>
        <p>Secure access to your agent workspace</p>
      </div>

      <form
        class="login-form"
        @submit.prevent="handleLogin"
      >
        <div class="form-field">
          <label>Master Access Key</label>
          <input
            v-model="masterKey"
            type="password"
            placeholder="Paste your master key"
            required
            autofocus
          >
        </div>

        <button
          type="submit"
          class="btn btn-save login-btn"
          :disabled="loading"
        >
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
/* Login-page specifics that aren't part of the global token system. The page
   chrome (.login-page, .login-card, .login-header, .login-form, .login-btn,
   .login-footer) is styled globally in style.css so the dark theme is
   consistent. Only the logo sizing and fade-in animation live here. */
.login-logo {
  width: 64px;
  height: 64px;
  margin: 0 auto 16px;
  border-radius: var(--r-lg);
  border: 1px solid var(--border);
}

.login-card {
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
</style>
