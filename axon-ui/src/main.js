import { createApp } from 'vue'
import './style.css'
import App from './App.vue'
import { preloadNodeIcons } from './lib/iconPreload.js'

createApp(App).mount('#app')

// Warm the cache for node icons so the canvas doesn't flash the 📦 fallback.
preloadNodeIcons()
