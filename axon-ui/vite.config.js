import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

export default defineConfig({
  plugins: [vue()],
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
  server: {
    proxy: {
      '/api': 'http://localhost:3000',
      '/ws': { target: 'ws://localhost:3000', ws: true },
    }
  },
  test: {
    // Component tests mount real Vue components (ServicesPage, ModelsPage, ...),
    // which needs a DOM. happy-dom is lighter/faster than jsdom for this size
    // of suite.
    environment: 'happy-dom',
  },
})
