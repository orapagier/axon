import { createApp, h } from 'vue'
import ConfirmDialog from './src/components/ConfirmDialog.vue'
import PromptDialog from './src/components/PromptDialog.vue'
import { confirmDialog } from './src/lib/confirm.js'
import { promptDialog } from './src/lib/prompt.js'

const App = {
  setup() {
    const result = { value: '' }
    async function runConfirm() {
      const ok = await confirmDialog('This file will be permanently deleted. This action cannot be undone.', {
        title: 'Delete File',
        confirmText: 'Delete',
      })
      document.getElementById('result').textContent = 'confirm result: ' + ok
    }
    async function runPrompt() {
      const label = await promptDialog(
        'Keep this version from being pruned by giving it a label. Leave blank to clear.',
        'v3',
        { title: 'Label Version', placeholder: 'e.g. Before refactor' }
      )
      document.getElementById('result').textContent = 'prompt result: ' + JSON.stringify(label)
    }
    return () =>
      h('div', [
        h('button', { class: 'btn btn-danger', id: 'open-confirm', onClick: runConfirm }, 'Delete file'),
        h('button', { class: 'btn btn-primary', id: 'open-prompt', onClick: runPrompt, style: 'margin-left:12px' }, 'Label version'),
        h('div', { id: 'result', style: 'margin-top:20px;color:#fff' }, ''),
        h(ConfirmDialog),
        h(PromptDialog),
      ])
  },
}

createApp(App).mount('#app')
