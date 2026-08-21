import pluginVue from 'eslint-plugin-vue'
import globals from 'globals'

export default [
  {
    ignores: ['dist/**', 'node_modules/**'],
  },
  ...pluginVue.configs['flat/recommended'],
  {
    languageOptions: {
      ecmaVersion: 'latest',
      sourceType: 'module',
      globals: {
        ...globals.browser,
        ...globals.node,
      },
    },
    rules: {
      // Vue Flow node/edge components and a few premium-UI patterns use
      // multi-word-looking names that are still fine as single-file
      // component names in this codebase (e.g. page-level components).
      'vue/multi-word-component-names': 'off',
    },
  },
  {
    // The node properties panel edits the canvas node in place: the parent
    // hands it the live `selectedNode` object, every field is bound with
    // `v-model="node.data.config.*"`, and `onSettingsChange` writes to
    // `props.node.data` before emitting `save` so the parent persists the very
    // object it passed down. That is the deliberate contract here, not an
    // oversight — and it produced 89 errors that buried every real one in this
    // file. Scoped to this component so the rule keeps protecting the rest.
    files: ['src/components/NodeDetails.vue'],
    rules: {
      'vue/no-mutating-props': 'off',
    },
  },
]
